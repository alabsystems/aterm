// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One `unsafe fn` per C prototype — the typed [`msg`] casts every send needs.
//!
//! Each helper is pure runtime: none names a framework class, selector or
//! symbol, so they belong here rather than in a binding layer, and
//! `aterm-gui` (re-exported from `appkit`) and `crates/aterm-window` share one
//! definition of each prototype.
//!
//! # Ownership
//!
//! None of these retains, releases or autoreleases. The caller decides: an
//! `alloc`/`new`/`copy`/`mutableCopy` result goes into [`Obj`] or
//! [`Retained`]; anything else is borrowed until the enclosing
//! [`autoreleasepool`] pops.
//!
//! # Safety
//!
//! Every function is `unsafe` because the caller asserts that `sel` has the C
//! prototype the function's name spells and that `recv` is a live receiver
//! that responds to it. The wrong helper corrupts registers exactly as a wrong
//! hand-written cast would.
//!
//! # The prototype check
//!
//! A debug build checks that assertion at every send: each helper casts through
//! [`typed`], which compares its prototype with the runtime's own
//! `method_getTypeEncoding` for `sel` on `recv`'s class and panics, naming both,
//! when they differ in a way the call ABI can feel ([`abi_shape`]). That is
//! how a signed `-requestUserAttention:` return read as unsigned was found, and
//! no compiler sees it: the selector's types are a runtime fact.

use std::ffi::CStr;

use crate::{Bool, CGPoint, CGRect, CGSize, ClassPtr, Encode, Id, MsgFn, Sel, msg};

/// A `(self, _cmd, …)` send prototype whose every type has an encoding: the
/// method type string it should match, offsets aside.
trait Prototype: MsgFn {
    fn types() -> String;
}

macro_rules! impl_prototype {
    ($($arg:ident),*) => {
        impl<Ret: Encode, $($arg: Encode),*> Prototype
            for unsafe extern "C-unwind" fn(Id, Sel, $($arg),*) -> Ret
        {
            fn types() -> String {
                [Ret::ENCODING, "@:", $(<$arg as Encode>::ENCODING),*].concat()
            }
        }
    };
}
impl_prototype!();
impl_prototype!(A0);
impl_prototype!(A0, A1);
impl_prototype!(A0, A1, A2);
impl_prototype!(A0, A1, A2, A3);
impl_prototype!(A0, A1, A2, A3, A4);
impl_prototype!(A0, A1, A2, A3, A4, A5);
impl_prototype!(A0, A1, A2, A3, A4, A5, A6);
impl_prototype!(A0, A1, A2, A3, A4, A5, A6, A7);
impl_prototype!(A0, A1, A2, A3, A4, A5, A6, A7, A8);
impl_prototype!(A0, A1, A2, A3, A4, A5, A6, A7, A8, A9);

/// [`msg`] for `sel` on `recv`, checked in a debug build against the runtime's
/// encoding of the method (the module's prototype check).
///
/// # Safety
/// As [`msg`].
#[inline]
unsafe fn typed<F: Prototype>(recv: Id, sel: Sel) -> F
where
    F::Ret: Encode,
{
    #[cfg(debug_assertions)]
    // SAFETY: `recv` is live or nil, as every helper's caller asserts.
    unsafe {
        check_prototype(recv, sel, F::types)
    };
    // SAFETY: forwarded from the caller.
    unsafe { msg() }
}

/// Panic when the method `recv`'s class runs for `sel` has a prototype other
/// than `expected()` in a way the call can feel. A nil receiver, or one whose
/// class answers `sel` only by forwarding, has no method to compare.
///
/// # Safety
/// `recv` must be live or nil.
#[cfg(debug_assertions)]
unsafe fn check_prototype(recv: Id, sel: Sel, expected: fn() -> String) {
    if recv.is_null() {
        return;
    }
    // SAFETY: `recv` is live; its class is immortal.
    let cls = unsafe { crate::class_of(recv) };
    // SAFETY: `cls` is live; an unimplemented selector yields `None`.
    let Some(runtime) = (unsafe { crate::method_types(cls, sel) }) else {
        return;
    };
    let runtime = crate::strip_method_offsets(&runtime);
    let ours = expected();
    if abi_shape(&runtime) != abi_shape(&ours) {
        // SAFETY: `cls` is a live class.
        let name: &CStr = unsafe { crate::class_name(cls) };
        if swift_override_of_the_same_width(&name.to_string_lossy(), &runtime, &ours) {
            return;
        }
        panic!(
            "-[{} {sel:?}] is `{runtime}` in the runtime, but this send casts it to `{ours}`",
            name.to_string_lossy()
        );
    }
}

/// A Swift class (`_Tt…`) that overrides a Foundation method encodes Swift's
/// `Int` where the Objective-C declaration says `NSUInteger`: `+[NSScreen
/// screens]` is a `ContiguousArrayStorage<NSScreen>` on macOS 27, whose `count`
/// is `q@:` while `-[NSArray count]` is `Q@:`. The send follows the declaration,
/// so for such a class a 32- or 64-bit integer's sign is not compared.
fn swift_override_of_the_same_width(class: &str, runtime: &str, ours: &str) -> bool {
    let sign_blind = |types: &str| -> Vec<String> {
        abi_shape(types)
            .into_iter()
            .map(|t| match t.as_str() {
                "Q" => "q".to_string(),
                "L" => "l".to_string(),
                "I" => "i".to_string(),
                _ => t,
            })
            .collect()
    };
    class.starts_with("_Tt") && sign_blind(runtime) == sign_blind(ours)
}

/// The parts of a method type string a send's ABI depends on, one entry per
/// type: every pointer (object, class, selector, block, C string, `^…`) is one
/// `p`, since each travels in a general register; integers keep width and
/// sign, floats their width, structs and unions their layout. Qualifiers
/// (`r` const, `V` oneway, the `n`/`o`/`N`/`O`/`R` DO markers) go.
fn abi_shape(types: &str) -> Vec<String> {
    let b = types.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b"rnNoORV".contains(&b[i]) {
            i += 1;
            continue;
        }
        let end = type_end(b, i);
        out.push(match b[i] {
            b'@' | b'#' | b':' | b'*' | b'^' => "p".to_string(),
            _ => types[i..end].to_string(),
        });
        i = end;
    }
    out
}

/// The index just past the one type starting at `b[i]`.
fn type_end(b: &[u8], i: usize) -> usize {
    match b.get(i) {
        None => i,
        Some(b'^') => type_end(b, i + 1),
        Some(q) if b"rnNoORV".contains(q) => type_end(b, i + 1),
        // `@?` is a block; `@"Name"` an object of a named class.
        Some(b'@') => match b.get(i + 1) {
            Some(b'?') => i + 2,
            Some(b'"') => b[i + 2..]
                .iter()
                .position(|&c| c == b'"')
                .map_or(b.len(), |q| i + 3 + q),
            _ => i + 1,
        },
        Some(&open @ (b'{' | b'(' | b'[')) => {
            let close = match open {
                b'{' => b'}',
                b'(' => b')',
                _ => b']',
            };
            let (mut depth, mut j, mut quoted) = (0_usize, i, false);
            while j < b.len() {
                match b[j] {
                    b'"' => quoted = !quoted,
                    c if !quoted && c == open => depth += 1,
                    c if !quoted && c == close => {
                        depth -= 1;
                        if depth == 0 {
                            return j + 1;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            b.len()
        }
        Some(_) => i + 1,
    }
}

/// `+alloc` on `cls` — a +1, zero-filled, uninitialised instance.
///
/// # Safety
/// `cls` must be a live, registered class. The result is +1 and the caller owes
/// it exactly one `-init…` and one eventual release.
#[must_use]
pub unsafe fn alloc(cls: ClassPtr) -> Id {
    // SAFETY: `+alloc` is `-(id)(Class, SEL)` on every class in the runtime.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = msg();
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
        let f: unsafe extern "C-unwind" fn(Id, Sel) = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id) = typed(recv, sel);
        f(recv, sel, a);
    }
}

/// `-(void)sel:(id)a other:(id)b`.
///
/// # Safety
/// See the module note.
pub unsafe fn send_v_id_id(recv: Id, sel: Sel, a: Id, b: Id) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Id) = typed(recv, sel);
        f(recv, sel, a, b);
    }
}

/// `-(void)sel:(BOOL)a`.
///
/// # Safety
/// See the module note. `a` is sent as a [`Bool`], never a Rust `bool`: on the
/// `x86_64-apple-darwin` compat slice `BOOL` is `signed char`.
pub unsafe fn send_v_bool(recv: Id, sel: Sel, a: bool) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Bool) = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, isize) = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, usize) = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGSize) = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Sel, Id) -> Id = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, f64) -> Id = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect) -> Id = typed(recv, sel);
        f(recv, sel, a)
    }
}

/// `-(BOOL)sel`.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_bool(recv: Id, sel: Sel) -> bool {
    // SAFETY: the caller pins the prototype; this is the cast for it. `Bool`
    // and not `bool`: `msg` refuses the latter, deliberately.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> Bool = typed(recv, sel);
        f(recv, sel).as_bool()
    }
}

/// `-(BOOL)sel:(SEL)a` — `-respondsToSelector:`.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_bool_sel(recv: Id, sel: Sel, a: Sel) -> bool {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> Bool = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Bool = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> isize = typed(recv, sel);
        f(recv, sel)
    }
}

/// `-(NSUInteger)sel`.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_usize(recv: Id, sel: Sel) -> usize {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> f64 = typed(recv, sel);
        f(recv, sel)
    }
}

/// `-(NSRect)sel`. A 32-byte return, so on x86_64 [`msg`] routes it through
/// `objc_msgSend_stret`.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_rect(recv: Id, sel: Sel) -> CGRect {
    // Nil receiver: libobjc zeroes register-class returns on the nil path but
    // never writes an indirect struct return, and a 32-byte CGRect is
    // `objc_msgSend_stret` on x86_64, so `[nil frame]` would return stack
    // garbage. clang makes the same check around every stret send.
    if recv.is_null() {
        return CGRect::default();
    }
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> CGRect = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, f64) = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGPoint) = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect) = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Id, Id) = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, isize, Id) = typed(recv, sel);
        f(recv, sel, a, b, c);
    }
}

/// `-(NSPoint)sel` — `-[NSEvent locationInWindow]`. Sixteen bytes of two
/// `f64`, returned directly on both architectures (unlike [`send_rect`]).
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_point(recv: Id, sel: Sel) -> CGPoint {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> CGPoint = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGPoint) -> CGPoint = typed(recv, sel);
        f(recv, sel, a)
    }
}

/// `-(NSPoint)sel:(NSPoint)a fromView:(id)b` — `-[NSView
/// convertPoint:fromView:]`, where a nil `b` means window coordinates.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_point_point_id(recv: Id, sel: Sel, a: CGPoint, b: Id) -> CGPoint {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGPoint, Id) -> CGPoint = typed(recv, sel);
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
    // Nil receiver: an indirect struct return is left uninitialised by libobjc
    // on x86_64 — see `send_rect`.
    if recv.is_null() {
        return CGRect::default();
    }
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect, Id) -> CGRect = typed(recv, sel);
        f(recv, sel, a, b)
    }
}

/// `-(id)sel:(NSUInteger)a` — `-[NSWindow standardWindowButton:]`, whose
/// argument is an `NSWindowButton`: an `NSUInteger` enum, not an `NSInteger`.
///
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_usize(recv: Id, sel: Sel, a: usize) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect, f64, f64) -> Id = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, f64, f64, f64, f64) -> Id = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect, usize, Id, Id) -> Id = typed(recv, sel);
        f(recv, sel, a, b, c, d)
    }
}

/// `-(id)sel:(id)a target:(id)b action:(SEL)c` — `+[NSButton
/// buttonWithTitle:target:action:]`. The mirror of [`send_id_id_sel_id`], with
/// `SEL` last.
///
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_id_id_sel(recv: Id, sel: Sel, a: Id, b: Id, c: Sel) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Id, Sel) -> Id = typed(recv, sel);
        f(recv, sel, a, b, c)
    }
}

// ---------------------------------------------------------------------------
// Shapes `crates/aterm-window` needs. Each carries the SDK declaration it was derived
// from, since the prototype is the safety obligation.
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect, Bool) = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect, Id) = typed(recv, sel);
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, isize) = typed(recv, sel);
        f(recv, sel, a, b);
    }
}

/// `-(void)sel:(id)a b:(id)b c:(NSUInteger)c d:(void *)d`.
///
/// `NSKeyValueObserving.h` — `-addObserver:(NSObject *)observer
/// forKeyPath:(NSString *)keyPath options:(NSKeyValueObservingOptions)options
/// context:(void *)context`. `context:` is `^v`, not `@`: a bare pointer AppKit
/// stores and hands back verbatim, never messaged or retained.
///
/// # Safety
/// See the module note. `d` is not dereferenced here and must satisfy whatever
/// the observing method assumes of it.
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Id, usize, *mut core::ffi::c_void) =
            typed(recv, sel);
        f(recv, sel, a, b, c, d);
    }
}

/// `-(void)sel:(SEL)a withObject:(id)b afterDelay:(NSTimeInterval)c`.
///
/// `NSRunLoop.h` — `-performSelector:withObject:afterDelay:`.
/// `NSTimeInterval` is `double`.
///
/// # Safety
/// See the module note. `-performSelector:withObject:afterDelay:` retains
/// `recv` until the run loop performs `a`.
pub unsafe fn send_v_sel_id_f64(recv: Id, sel: Sel, a: Sel, b: Id, c: f64) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Sel, Id, f64) = typed(recv, sel);
        f(recv, sel, a, b, c);
    }
}

/// `-(void)sel:(id)a selector:(SEL)b name:(id)c object:(id)d`.
///
/// `NSNotification.h` — `-addObserver:(id)observer selector:(SEL)aSelector
/// name:(NSNotificationName)aName object:(id)anObject`. The observer is
/// registered unretained; removing it before it dies is the caller's business.
///
/// # Safety
/// See the module note.
pub unsafe fn send_v_id_sel_id_id(recv: Id, sel: Sel, a: Id, b: Sel, c: Id, d: Id) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Sel, Id, Id) = typed(recv, sel);
        f(recv, sel, a, b, c, d);
    }
}

/// `+(id)sel:(const id *)a count:(NSUInteger)b`.
///
/// `NSArray.h` — `+arrayWithObjects:count:`, the counted form; a variadic send
/// would be the wrong shape on AAPCS64. `idptr`, not `ptr`: the argument
/// encodes `r^@` (Foundation reads and retains `b` elements), unlike the KVO
/// `context:` `^v` of [`send_v_id_id_usize_ptr`].
///
/// # Safety
/// See the module note, including the ownership paragraph. `a` must point to
/// at least `b` live object pointers, none of them nil.
#[must_use]
pub unsafe fn send_id_idptr_usize(recv: Id, sel: Sel, a: *const Id, b: usize) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, *const Id, usize) -> Id = typed(recv, sel);
        f(recv, sel, a, b)
    }
}

/// `-(NSRect)sel:(NSRect)a`.
///
/// `NSWindow.h` — `-contentRectForFrameRect:(NSRect)frameRect`;
/// `NSView.h` — `-convertRectToScreen:(NSRect)rect`.
///
/// On AAPCS64 both the argument and the return are homogeneous float
/// aggregates in `d0`-`d3`; on x86_64 the return is `_stret` (see
/// [`crate::returns_indirectly`]).
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_rect_rect(recv: Id, sel: Sel, a: CGRect) -> CGRect {
    // Nil receiver: an indirect struct return is left uninitialised by libobjc
    // on x86_64 — see `send_rect`.
    if recv.is_null() {
        return CGRect::default();
    }
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect) -> CGRect = typed(recv, sel);
        f(recv, sel, a)
    }
}

/// `-(NSInteger)sel:(NSUInteger)a` — `-[NSApplication requestUserAttention:]`.
///
/// Mixed signedness, as the runtime reports it:
///
/// ```text
/// NSApplication  requestUserAttention:  q24@0:8Q16
/// ```
///
/// The return is declared even though callers discard it, since a `()`
/// prototype is a different function type.
///
/// # Safety
/// See the module note.
pub unsafe fn send_isize_usize(recv: Id, sel: Sel, a: usize) -> isize {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, usize) -> isize = typed(recv, sel);
        f(recv, sel, a)
    }
}

/// `-(NSInteger)sel:(NSRect)a owner:(id)b userData:(void *)c assumeInside:(BOOL)d`.
///
/// `NSView.h` — `-addTrackingRect:owner:userData:assumeInside:`, returning an
/// `NSTrackingRectTag` (`NSInteger`). `userData` is `^v`, handed back verbatim
/// to the owner's mouse-entered/exited rows.
///
/// # Safety
/// See the module note. `b` is not retained, so it must outlive the tracking
/// rect or be removed first.
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
        let f: unsafe extern "C-unwind" fn(
            Id,
            Sel,
            CGRect,
            Id,
            *mut core::ffi::c_void,
            Bool,
        ) -> isize = typed(recv, sel);
        f(recv, sel, a, b, c, Bool::new(d))
    }
}

/// `-(unsigned short)sel`.
///
/// `NSEvent.h` — `-keyCode`. Not `NSUInteger`: AAPCS64 leaves the upper 48 bits
/// of `x0` unspecified for a `short` return.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_u16(recv: Id, sel: Sel) -> u16 {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> u16 = typed(recv, sel);
        f(recv, sel)
    }
}

/// `-(BOOL)sel:(NSInteger)a`.
///
/// `NSApplication.h` — `-setActivationPolicy:`. Callers ignore the answer, but
/// the return type is part of the prototype.
///
/// # Safety
/// See the module note.
pub unsafe fn send_bool_isize(recv: Id, sel: Sel, a: isize) -> bool {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, isize) -> Bool = typed(recv, sel);
        f(recv, sel, a).as_bool()
    }
}

/// `-(void)sel:(id)a atStart:(BOOL)b`.
///
/// `NSApplication.h` — `-postEvent:atStart:`, used to wake `-[NSApplication
/// run]` out of its event wait.
///
/// # Safety
/// See the module note.
pub unsafe fn send_v_id_bool(recv: Id, sel: Sel, a: Id, b: bool) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Bool) = typed(recv, sel);
        f(recv, sel, a, Bool::new(b));
    }
}

/// `-(unsigned int)sel`.
///
/// `NSValue.h` — `-unsignedIntValue`, read off `-[NSScreen deviceDescription]`'s
/// `@"NSScreenNumber"` to get a `CGDirectDisplayID` (`uint32_t`).
///
/// AAPCS64 leaves the upper 32 bits of `x0` unspecified for an `int` return;
/// in practice Foundation zeroes them, so this helper exists because the
/// runtime encodes `I` and the prototype census checks the letter.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_u32(recv: Id, sel: Sel) -> u32 {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> u32 = typed(recv, sel);
        f(recv, sel)
    }
}

/// `-(BOOL)sel:(Class)a`.
///
/// `NSObject.h` — `-isKindOfClass:(Class)aClass`. Takes a [`ClassPtr`] rather
/// than laundering one through [`ClassPtr::as_id`].
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_bool_cls(recv: Id, sel: Sel, a: ClassPtr) -> bool {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, ClassPtr) -> Bool = typed(recv, sel);
        f(recv, sel, a).as_bool()
    }
}

/// `+(id)keyEventWithType:location:modifierFlags:timestamp:windowNumber:`
/// `context:characters:charactersIgnoringModifiers:isARepeat:keyCode:`.
///
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
/// `NSEventType` and `NSEventModifierFlags` are `NSUInteger`; `NSTimeInterval`
/// is `double`; `keyCode` is `unsigned short` (see [`send_u16`]).
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
        let f: unsafe extern "C-unwind" fn(
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
        ) -> Id = typed(recv, sel);
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
/// `subtype:` is a signed `short` (`s`, not `S`), per the runtime:
///
/// ```text
/// @92@0:8Q16{CGPoint=dd}24Q40d48q56@64s72q76q84
/// ```
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
        let f: unsafe extern "C-unwind" fn(
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
        ) -> Id = typed(recv, sel);
        f(recv, sel, a, b, c, d, e, f_, g, h, i)
    }
}

// ---------------------------------------------------------------------------
// Shapes `cursor.rs` and `view.rs` need. `-magnification` is `d16@0:8` but
// `-rotation` is `f16@0:8`: on AAPCS64 a `float` return arrives in `s0`, so
// reading it with the `double` prototype yields garbage. Only
// `method_getTypeEncoding` tells them apart, hence [`send_f32`].
// ---------------------------------------------------------------------------

/// `-(float)sel` — a C `float`, not a `CGFloat`.
///
/// ```text
/// NSEvent  rotation  f16@0:8
/// NSEvent  pressure  f16@0:8
/// ```
///
/// Neighbouring properties (`-magnification`, `-scrollingDeltaX`,
/// `-timestamp`) are `double`; [`send_f64`] is the wrong prototype for these.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_f32(recv: Id, sel: Sel) -> f32 {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> f32 = typed(recv, sel);
        f(recv, sel)
    }
}

/// `-(id)sel:(SEL)a`.
///
/// `NSObject.h` — `-performSelector:`, used to reach the undocumented
/// `+[NSCursor _helpCursor]` family by name.
///
/// # Safety
/// See the module note, including the ownership paragraph. The result follows
/// the invoked method's convention; the `+…Cursor` accessors return +0.
#[must_use]
pub unsafe fn send_id_sel(recv: Id, sel: Sel, a: Sel) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> Id = typed(recv, sel);
        f(recv, sel, a)
    }
}

/// `-(NSUInteger)sel:(NSUInteger)a`.
///
/// `NSString.h` — `-lengthOfBytesUsingEncoding:` (`Q24@0:8Q16`): a UTF-8 byte
/// count, as opposed to `-length`'s UTF-16 code units. The IME pre-edit path
/// needs both, and confusing them misplaces the caret for non-ASCII text.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_usize_usize(recv: Id, sel: Sel, a: usize) -> usize {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, usize) -> usize = typed(recv, sel);
        f(recv, sel, a)
    }
}

/// `-(unsigned char *)sel` — an interior pointer into the receiver's storage.
///
/// `NSBitmapImageRep.h` — `-bitmapData`, encoded `*16@0:8` (`*` is the
/// runtime's `char *`, not `^C`).
///
/// # Safety
/// See the module note. The pointer is valid only while `recv` is alive and its
/// buffer is not reallocated; writes go into the receiver's pixels.
#[must_use]
pub unsafe fn send_charptr(recv: Id, sel: Sel) -> *mut core::ffi::c_uchar {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> *mut core::ffi::c_uchar = typed(recv, sel);
        f(recv, sel)
    }
}

/// `-(id)sel:(const void *)a length:(NSUInteger)b`.
///
/// `NSData.h` — `-initWithBytes:length:`, encoded `@32@0:8r^v16Q24`. `cptr`
/// (`r^v`, read and copied) as opposed to `ptr` (`^v`, stored untouched).
///
/// # Safety
/// See the module note, including the ownership paragraph — an initialiser, so
/// the result is +1. `a` must point to at least `b` readable bytes for the
/// call; they are copied.
#[must_use]
pub unsafe fn send_id_cptr_usize(recv: Id, sel: Sel, a: *const core::ffi::c_void, b: usize) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, *const core::ffi::c_void, usize) -> Id =
            typed(recv, sel);
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
/// As the runtime encodes it:
///
/// ```text
/// @88@0:8^*16q24q32q40q48B56B60@64q72q80
/// ```
///
/// Four arguments go on the stack on AAPCS64, and the two `B`s are single
/// bytes the caller must not widen. `planeptr` is `^*`, an array of plane
/// pointers (distinct from `ptr` `^v` and `cptr` `r^v`); null asks the receiver
/// to allocate its own buffer.
///
/// # Safety
/// See the module note, including the ownership paragraph — an initialiser, so
/// the result is +1. `a` must be null or point to `spp` plane pointers; `h`
/// must be a live `NSColorSpaceName` (an `NSString`).
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
        let f: unsafe extern "C-unwind" fn(
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
        ) -> Id = typed(recv, sel);
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
/// `NSImage.h` — `-initWithSize:`, encoded `@32@0:8{CGSize=dd}16`; passed in
/// `d0`/`d1` (AAPCS64) or `xmm0`/`xmm1` (x86_64).
///
/// # Safety
/// See the module note, including the ownership paragraph — an initialiser, so
/// the result is +1.
#[must_use]
pub unsafe fn send_id_size(recv: Id, sel: Sel, a: CGSize) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGSize) -> Id = typed(recv, sel);
        f(recv, sel, a)
    }
}

/// `-(id)sel:(id)a hotSpot:(NSPoint)b`.
///
/// `NSCursor.h` — `-initWithImage:hotSpot:`, encoded `@40@0:8@16{CGPoint=dd}24`.
///
/// # Safety
/// See the module note, including the ownership paragraph — an initialiser, so
/// the result is +1. `a` must be a live `NSImage`; the receiver retains it.
#[must_use]
pub unsafe fn send_id_id_point(recv: Id, sel: Sel, a: Id, b: CGPoint) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, CGPoint) -> Id = typed(recv, sel);
        f(recv, sel, a, b)
    }
}

/// `-(id)sel:(id)a withInteger:(NSInteger)b`.
///
/// `NSBitmapImageRep.h` —
/// `-bitmapImageRepByConvertingToColorSpace:renderingIntent:`, encoded
///
/// ```text
/// @32@0:8@16q24
/// ```
///
/// `NSColorRenderingIntent` is an `NSInteger`. [`send_id_usize_id`] has the
/// same argument kinds in the other order with the other signedness.
///
/// # Safety
/// See the module note, including the ownership paragraph — the result is +0
/// and borrowed. `a` must be a live object of the class the selector expects.
#[must_use]
pub unsafe fn send_id_id_isize(recv: Id, sel: Sel, a: Id, b: isize) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, isize) -> Id = typed(recv, sel);
        f(recv, sel, a, b)
    }
}

/// `-(id)sel:(NSUInteger)a withObject:(id)b`.
///
/// `NSBitmapImageRep.h` — `-representationUsingType:properties:`, encoded
///
/// ```text
/// @32@0:8Q16@24
/// ```
///
/// `NSBitmapImageFileType` is an `NSUInteger`; see [`send_id_id_isize`].
///
/// # Safety
/// See the module note, including the ownership paragraph — the result is +0
/// and borrowed. `b` must be nil or a live object of the expected class.
#[must_use]
pub unsafe fn send_id_usize_id(recv: Id, sel: Sel, a: usize, b: Id) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, usize, Id) -> Id = typed(recv, sel);
        f(recv, sel, a, b)
    }
}

/// `-(void)sel:(void *)a length:(NSUInteger)b`.
///
/// `NSData.h` — `-getBytes:length:`, encoded
///
/// ```text
/// v32@0:8^v16Q24
/// ```
///
/// `^v`, not `r^v`: the receiver writes the buffer (the opposite direction of
/// [`send_id_cptr_usize`]).
///
/// # Safety
/// See the module note. `a` must point at `b` writable bytes, and `b` must not
/// exceed the receiver's length (`NSData` raises rather than truncating).
pub unsafe fn send_v_ptr_usize(recv: Id, sel: Sel, a: *mut core::ffi::c_void, b: usize) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, *mut core::ffi::c_void, usize) =
            typed(recv, sel);
        f(recv, sel, a, b);
    }
}

// ---------------------------------------------------------------------------
// Shapes the Developer Tools helper panel needs (`aterm-gui`'s
// `devtools_helper.rs`). Each carries the SDK declaration it was derived from;
// a debug build checks each send against the runtime's own encoding (`typed`).
// ---------------------------------------------------------------------------

/// `-(id)sel:(NSRect)a styleMask:(NSUInteger)b backing:(NSUInteger)c defer:(BOOL)d`.
///
/// `NSWindow.h` — `-initWithContentRect:(NSRect)contentRect
/// styleMask:(NSWindowStyleMask)style backing:(NSBackingStoreType)backingStoreType
/// defer:(BOOL)flag`, encoded
///
/// ```text
/// @68@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16Q48Q56B64
/// ```
///
/// Both enums are `NSUInteger`. `vendor/winit`'s window delegate sends the same
/// selector through its own longhand cast.
///
/// # Safety
/// See the module note, including the ownership paragraph: as an initializer it
/// consumes the receiver's +1 from `+alloc` and returns +1 (or nil, having
/// released it).
#[must_use]
pub unsafe fn send_id_rect_usize_usize_bool(
    recv: Id,
    sel: Sel,
    a: CGRect,
    b: usize,
    c: usize,
    d: bool,
) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect, usize, usize, Bool) -> Id =
            typed(recv, sel);
        f(recv, sel, a, b, c, Bool::new(d))
    }
}

/// `-(id)sel:(id)a event:(id)b source:(id)c`.
///
/// `NSView.h` — `-beginDraggingSessionWithItems:(NSArray<NSDraggingItem *> *)items
/// event:(NSEvent *)event source:(id<NSDraggingSource>)source`, encoded
///
/// ```text
/// @40@0:8@16@24@32
/// ```
///
/// # Safety
/// See the module note, including the ownership paragraph — the result is +0
/// and borrowed.
#[must_use]
pub unsafe fn send_id_id_id_id(recv: Id, sel: Sel, a: Id, b: Id, c: Id) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Id, Id) -> Id = typed(recv, sel);
        f(recv, sel, a, b, c)
    }
}

/// `-(void)sel:(SEL)a`.
///
/// `NSControl.h` — `@property (nullable) SEL action`, i.e. `-setAction:(SEL)`,
/// encoded
///
/// ```text
/// v24@0:8:16
/// ```
///
/// # Safety
/// See the module note. AppKit stores the selector and sends it to the target
/// later; that the target answers it is the caller's obligation.
pub unsafe fn send_v_sel(recv: Id, sel: Sel, a: Sel) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Sel) = typed(recv, sel);
        f(recv, sel, a);
    }
}

/// `-(NSSize)sel`.
///
/// `NSCell.h` — `@property (readonly) NSSize cellSize`, encoded
///
/// ```text
/// {CGSize=dd}16@0:8
/// ```
///
/// Two `double`s come back in registers on both Apple ABIs (an HFA on arm64,
/// `xmm0`/`xmm1` on x86_64), so [`msg`] picks plain `objc_msgSend`.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_size(recv: Id, sel: Sel) -> CGSize {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> CGSize = typed(recv, sel);
        f(recv, sel)
    }
}

/// `-(NSSize)sel:(NSRect)a`.
///
/// `NSCell.h` — `-cellSizeForBounds:(NSRect)rect`, encoded
///
/// ```text
/// {CGSize=dd}48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16
/// ```
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_size_rect(recv: Id, sel: Sel, a: CGRect) -> CGSize {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect) -> CGSize = typed(recv, sel);
        f(recv, sel, a)
    }
}

/// `-(int)sel`.
///
/// `NSRunningApplication.h` — `@property (readonly) pid_t processIdentifier`,
/// encoded
///
/// ```text
/// i16@0:8
/// ```
///
/// `pid_t` is `int`: 32 bits and SIGNED, not `NSInteger` and not the unsigned
/// [`send_u32`].
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_i32(recv: Id, sel: Sel) -> i32 {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> i32 = typed(recv, sel);
        f(recv, sel)
    }
}

/// `-(BOOL)sel:(id)a forType:(id)b`.
///
/// `NSPasteboardItem.h` — `-setString:(NSString *)string
/// forType:(NSPasteboardType)type`, encoded
///
/// ```text
/// B32@0:8@16@24
/// ```
///
/// (`c` for the `BOOL` on x86_64, which [`Bool`] carries.)
///
/// # Safety
/// See the module note.
pub unsafe fn send_bool_id_id(recv: Id, sel: Sel, a: Id, b: Id) -> bool {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Id) -> Bool = typed(recv, sel);
        f(recv, sel, a, b).as_bool()
    }
}

/// `-(id)sel:(id)a isDirectory:(BOOL)b`.
///
/// `NSURL.h` — `+fileURLWithPath:(NSString *)path isDirectory:(BOOL)isDir`,
/// encoded
///
/// ```text
/// @28@0:8@16B24
/// ```
///
/// # Safety
/// See the module note, including the ownership paragraph — the result is +0
/// and borrowed.
#[must_use]
pub unsafe fn send_id_id_bool(recv: Id, sel: Sel, a: Id, b: bool) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Bool) -> Id = typed(recv, sel);
        f(recv, sel, a, Bool::new(b))
    }
}

/// `-(BOOL)sel:(NSUInteger)a`.
///
/// `NSRunningApplication.h` —
/// `-activateWithOptions:(NSApplicationActivationOptions)options`, encoded
///
/// ```text
/// B24@0:8Q16
/// ```
///
/// The options are an `NSUInteger` bitmask; [`send_bool_isize`] is its signed
/// twin.
///
/// # Safety
/// See the module note.
pub unsafe fn send_bool_usize(recv: Id, sel: Sel, a: usize) -> bool {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Bool = typed(recv, sel);
        f(recv, sel, a).as_bool()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same(a: &str, b: &str) -> bool {
        abi_shape(a) == abi_shape(b)
    }

    #[test]
    fn the_shape_keeps_sign_width_and_layout_and_merges_pointers() {
        // `-requestUserAttention:`: a signed return read as unsigned.
        assert!(!same("q@:Q", "Q@:Q"));
        assert!(!same("f@:", "d@:"));
        assert!(!same("S@:", "Q@:"));
        assert!(!same("v@:{CGSize=dd}", "v@:{CGPoint=dd}"));
        assert!(!same("v@:@", "v@:@@"), "the arity counts");
        // Every pointer kind travels in one general register.
        assert!(same("v@:^{CGColor=}", "v@:@"));
        assert!(same("v@:@?", "v@:@"));
        assert!(same("@@:r^@Q", "@@:^@Q"));
        assert!(same("B@:#", "B@:@"));
        assert!(same("Vv@:", "v@:"));
        assert!(same("v@:@\"NSString\"", "v@:@"));
        assert!(same("@@:^*qqqqBB@qq", "@@:^*qqqqBB@qq"));
        assert_eq!(
            abi_shape("{CGRect={CGPoint=dd}{CGSize=dd}}@:[4i]^^v"),
            ["{CGRect={CGPoint=dd}{CGSize=dd}}", "p", "p", "[4i]", "p"]
        );
    }

    #[test]
    fn a_swift_override_may_differ_only_in_a_wide_integers_sign() {
        let swift = "_TtGCs23_ContiguousArrayStorageCSo8NSScreen_$";
        assert!(swift_override_of_the_same_width(swift, "q@:", "Q@:"));
        assert!(swift_override_of_the_same_width(swift, "v@:i", "v@:I"));
        // never for an Objective-C class, a narrow integer or another width
        assert!(!swift_override_of_the_same_width(
            "NSWindow", "q@:Q", "Q@:Q"
        ));
        assert!(!swift_override_of_the_same_width(swift, "v@:s", "v@:S"));
        assert!(!swift_override_of_the_same_width(swift, "q@:", "I@:"));
    }

    /// A live send: the right prototype passes, the wrong sign panics before
    /// the message is sent, naming both encodings.
    #[test]
    #[cfg(debug_assertions)]
    fn a_send_cast_to_the_wrong_prototype_panics_in_a_debug_build() {
        let s = crate::ns_string("four").expect("NSString");
        // SAFETY: `-length` is `-(NSUInteger)length` on a live NSString.
        assert_eq!(unsafe { send_usize(s.id(), crate::sel!(length)) }, 4);
        let wrong = std::panic::catch_unwind(|| {
            // SAFETY: never sent: the check refuses the signed prototype first.
            unsafe { send_isize(s.id(), crate::sel!(length)) }
        });
        let message = wrong.expect_err("a signed read of `-length` must panic");
        let message = message.downcast_ref::<String>().expect("a formatted panic");
        assert!(
            message.contains("length") && message.contains("`Q@:`") && message.contains("`q@:`"),
            "{message}"
        );
    }
}
