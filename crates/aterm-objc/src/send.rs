// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One `unsafe fn` per C prototype — the typed sends every call site needs.
//! # Safety
//! Every function is `unsafe` because the caller asserts that `sel` has the C prototype the function's name spells and that `recv` is a live receiver that responds to it. The wrong helper corrupts registers exactly as a wrong hand-written cast would.

use std::ffi::CStr;

use crate::{Bool, CGPoint, CGRect, CGSize, ClassPtr, Encode, Id, MsgFn, Sel};

/// What a checked send may be addressed to: an object, or a class.
/// # Safety
/// `receiver_id` must return the receiver unchanged, as the object pointer the runtime dispatches on.
pub unsafe trait Receiver: Copy + Encode {
    /// The receiver as the object the runtime dispatches on.
    fn receiver_id(self) -> Id;
}

// SAFETY: the identity.
unsafe impl Receiver for Id {
    fn receiver_id(self) -> Id {
        self
    }
}

// SAFETY: a class is an object; `as_id` is the same pointer.
unsafe impl Receiver for ClassPtr {
    fn receiver_id(self) -> Id {
        self.as_id()
    }
}

/// A send prototype [`checked_msg`] can build.
/// # Safety
/// Implemented here for exactly those function-pointer types; `get` returns a pointer of the implementing type whose call sends the message.
pub unsafe trait CheckedFn: MsgFn {
    /// The cast: a checking trampoline in a debug build, the bare `objc_msgSend` cast in a release build.
    #[doc(hidden)]
    fn get() -> Self;

    /// The `(self, _cmd, …)` method type this prototype sends, offsets aside.
    #[doc(hidden)]
    fn types() -> String;
}

/// [`crate::msg`], checked in a debug build at every call against the runtime's encoding of the method the receiver's class runs for the selector.
/// # Safety
/// As [`crate::msg`]: `F` must be the exact C prototype of every selector the pointer is called with.
#[inline]
#[must_use]
pub unsafe fn checked_msg<F: CheckedFn>() -> F {
    F::get()
}

fn types_of(ret: &str, args: &[&str]) -> String {
    let mut t = String::with_capacity(8 + args.len() * 4);
    t.push_str(ret);
    t.push_str("@:");
    for a in args {
        t.push_str(a);
    }
    t
}

macro_rules! impl_checked_fn {
    ($($arg:ident $val:ident),*) => {
        // SAFETY: an `unsafe extern "C-unwind"` function pointer; both arms of `get` return one of exactly this type.
        unsafe impl<Recv: Receiver, Ret: Encode, $($arg: Encode),*> CheckedFn
            for unsafe extern "C-unwind" fn(Recv, Sel, $($arg),*) -> Ret
        {
            fn types() -> String {
                types_of(Ret::ENCODING, &[$(<$arg as Encode>::ENCODING),*])
            }

            #[inline]
            fn get() -> Self {
                #[cfg(debug_assertions)]
                {
                    unsafe extern "C-unwind" fn trampoline<Recv: Receiver, Ret: Encode, $($arg: Encode),*>(
                        recv: Recv,
                        sel: Sel,
                        $($val: $arg),*
                    ) -> Ret {
                        let this: unsafe extern "C-unwind" fn(Recv, Sel, $($arg),*) -> Ret =
                            trampoline::<Recv, Ret, $($arg),*>;
                        // SAFETY: the caller of `checked_msg` asserted that `recv` is live or nil.
                        unsafe {
                            check(
                                recv.receiver_id(),
                                sel,
                                this as usize,
                                <unsafe extern "C-unwind" fn(Recv, Sel, $($arg),*) -> Ret as CheckedFn>::types,
                            );
                        }
                        #[expect(
                            clippy::disallowed_methods,
                            reason = "the checked send's own cast, after the check"
                        )]
                        // SAFETY: forwarded from the caller of `checked_msg`; the prototype is this trampoline's own.
                        let send: unsafe extern "C-unwind" fn(Recv, Sel, $($arg),*) -> Ret =
                            unsafe { crate::msg() };
                        // SAFETY: as above.
                        unsafe { send(recv, sel, $($val),*) }
                    }
                    trampoline::<Recv, Ret, $($arg),*>
                }
                #[cfg(not(debug_assertions))]
                {
                    #[expect(
                        clippy::disallowed_methods,
                        reason = "a release build's checked send is the bare cast"
                    )]
                    // SAFETY: the caller of `checked_msg` pins the prototype.
                    unsafe {
                        crate::msg()
                    }
                }
            }
        }
    };
}
impl_checked_fn!();
impl_checked_fn!(A0 a0);
impl_checked_fn!(A0 a0, A1 a1);
impl_checked_fn!(A0 a0, A1 a1, A2 a2);
impl_checked_fn!(A0 a0, A1 a1, A2 a2, A3 a3);
impl_checked_fn!(A0 a0, A1 a1, A2 a2, A3 a3, A4 a4);
impl_checked_fn!(A0 a0, A1 a1, A2 a2, A3 a3, A4 a4, A5 a5);
impl_checked_fn!(A0 a0, A1 a1, A2 a2, A3 a3, A4 a4, A5 a5, A6 a6);
impl_checked_fn!(A0 a0, A1 a1, A2 a2, A3 a3, A4 a4, A5 a5, A6 a6, A7 a7);
impl_checked_fn!(A0 a0, A1 a1, A2 a2, A3 a3, A4 a4, A5 a5, A6 a6, A7 a7, A8 a8);
impl_checked_fn!(A0 a0, A1 a1, A2 a2, A3 a3, A4 a4, A5 a5, A6 a6, A7 a7, A8 a8, A9 a9);
impl_checked_fn!(A0 a0, A1 a1, A2 a2, A3 a3, A4 a4, A5 a5, A6 a6, A7 a7, A8 a8, A9 a9, A10 a10);
impl_checked_fn!(
    A0 a0, A1 a1, A2 a2, A3 a3, A4 a4, A5 a5, A6 a6, A7 a7, A8 a8, A9 a9, A10 a10, A11 a11
);
impl_checked_fn!(
    A0 a0, A1 a1, A2 a2, A3 a3, A4 a4, A5 a5, A6 a6, A7 a7, A8 a8, A9 a9, A10 a10, A11 a11,
    A12 a12
);
impl_checked_fn!(
    A0 a0, A1 a1, A2 a2, A3 a3, A4 a4, A5 a5, A6 a6, A7 a7, A8 a8, A9 a9, A10 a10, A11 a11,
    A12 a12, A13 a13
);

#[cfg(debug_assertions)]
type Passed =
    std::collections::HashSet<(usize, usize, usize), std::hash::BuildHasherDefault<AddrHasher>>;

#[cfg(debug_assertions)]
std::thread_local! {
    static PASSED: std::cell::RefCell<Passed> = std::cell::RefCell::new(Passed::default());
}

#[cfg(debug_assertions)]
#[derive(Default)]
struct AddrHasher(u64);

#[cfg(debug_assertions)]
impl std::hash::Hasher for AddrHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(u64::from(b));
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0.rotate_left(5) ^ n).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }

    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// # Safety
/// `recv` must be live or nil.
#[cfg(debug_assertions)]
unsafe fn check(recv: Id, sel: Sel, proto: usize, expected: fn() -> String) {
    if recv.is_null() {
        return;
    }
    // SAFETY: `recv` is live; its class is immortal.
    let cls = unsafe { crate::class_of(recv) };
    let key = (cls.addr(), sel.as_ptr().addr(), proto);
    if PASSED.try_with(|p| p.borrow().contains(&key)) == Ok(true) {
        return;
    }
    // SAFETY: `cls` is live; an unimplemented selector yields `None`.
    let Some(runtime) = (unsafe { crate::method_types(cls, sel) }) else {
        return;
    };
    let runtime = crate::strip_method_offsets(&runtime);
    // SAFETY: `cls` is a live class.
    if let Some(why) = unsafe { mismatch(cls, sel, &runtime, &expected()) } {
        panic!("{why}");
    }
    let _ = PASSED.try_with(|p| p.borrow_mut().insert(key));
}

/// # Safety
/// `cls` must be a live class.
unsafe fn mismatch(cls: ClassPtr, sel: Sel, runtime: &str, ours: &str) -> Option<String> {
    if abi_shape(runtime) == abi_shape(ours) {
        return None;
    }
    // SAFETY: `cls` is a live class.
    let name: &CStr = unsafe { crate::class_name(cls) };
    let name = name.to_string_lossy();
    if swift_override_of_the_same_width(&name, runtime, ours) {
        return None;
    }
    Some(format!(
        "-[{name} {sel:?}] is `{runtime}` in the runtime, but this send casts it to `{ours}`"
    ))
}

/// THE PROTOTYPE CHECK, AHEAD OF TIME.
/// # Safety
/// `cls` must be a live class object.
pub unsafe fn prototype_check<F: CheckedFn>(cls: ClassPtr, sel: Sel) -> Result<(), String> {
    // SAFETY: `cls` is live; an unimplemented selector yields `None`.
    let Some(runtime) = (unsafe { crate::method_types(cls, sel) }) else {
        return Err(format!(
            "{} runs no method for {sel:?}",
            // SAFETY: `cls` is a live class.
            unsafe { crate::class_name(cls) }.to_string_lossy()
        ));
    };
    let runtime = crate::strip_method_offsets(&runtime);
    // SAFETY: `cls` is a live class.
    match unsafe { mismatch(cls, sel, &runtime, &F::types()) } {
        Some(why) => Err(why),
        None => Ok(()),
    }
}

/// The signatures an extended method type encoding records for its block arguments, in order, each the text between `@?<` and its `>`.
#[must_use]
pub fn block_signatures(extended: &str) -> Vec<String> {
    let b = extended.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                i = b[i + 1..]
                    .iter()
                    .position(|&c| c == b'"')
                    .map_or(b.len(), |q| i + q + 2);
            }
            b'@' if b.get(i + 1) == Some(&b'?') && b.get(i + 2) == Some(&b'<') => {
                let end = angle_end(b, i + 2);
                let close = if b.get(end.wrapping_sub(1)) == Some(&b'>') {
                    end - 1
                } else {
                    end
                };
                out.push(extended[i + 3..close].to_string());
                i = end;
            }
            _ => i += 1,
        }
    }
    out
}

/// Whether a block of prototype `P` is the block `signature` describes (one of [`block_signatures`]), in every way the call ABI can feel.
pub fn block_check<P: crate::BlockFn>(signature: &str) -> Result<(), String> {
    let theirs = crate::strip_method_offsets(signature);
    let ours = P::types();
    if abi_shape(&theirs) == abi_shape(&ours) {
        Ok(())
    } else {
        Err(format!(
            "the block is `{theirs}` in the runtime, but this prototype is `{ours}`"
        ))
    }
}

const fn angle_end(b: &[u8], open: usize) -> usize {
    let (mut depth, mut j, mut quoted) = (0_usize, open, false);
    while j < b.len() {
        match b[j] {
            b'"' => quoted = !quoted,
            b'<' if !quoted => depth += 1,
            b'>' if !quoted => {
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

fn swift_override_of_the_same_width(class: &str, runtime: &str, ours: &str) -> bool {
    let blind =
        |types: &str| -> Vec<String> { abi_shape(types).iter().map(|t| sign_blind(t)).collect() };
    let swift = class.starts_with("_Tt") || class.contains('.');
    swift && blind(runtime) == blind(ours)
}

fn sign_blind(t: &str) -> String {
    let b = t.as_bytes();
    let mut out = String::with_capacity(t.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            open @ (b'{' | b'(') => {
                out.push(char::from(open));
                let end = b[i + 1..]
                    .iter()
                    .position(|c| b"={}()".contains(c))
                    .map_or(b.len(), |n| i + 1 + n);
                i = if b.get(end) == Some(&b'=') {
                    end + 1
                } else {
                    end
                };
                continue;
            }
            b'"' => {
                i = b[i + 1..]
                    .iter()
                    .position(|&c| c == b'"')
                    .map_or(b.len(), |n| i + n + 2);
                continue;
            }
            b'Q' => out.push('q'),
            b'L' => out.push('l'),
            b'I' => out.push('i'),
            b'B' => out.push('c'),
            c => out.push(char::from(c)),
        }
        i += 1;
    }
    out
}

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

fn type_end(b: &[u8], i: usize) -> usize {
    match b.get(i) {
        None => i,
        Some(b'^') => type_end(b, i + 1),
        Some(q) if b"rnNoORV".contains(q) => type_end(b, i + 1),
        Some(b'@') => match b.get(i + 1) {
            Some(b'?') if b.get(i + 2) == Some(&b'<') => angle_end(b, i + 2),
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
/// # Safety
/// `cls` must be a live, registered class. The result is +1 and the caller owes it exactly one `-init…` and one eventual release.
#[must_use]
pub unsafe fn alloc(cls: ClassPtr) -> Id {
    // SAFETY: `+alloc` is `-(id)(Class, SEL)` on every class in the runtime.
    unsafe {
        let f: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id = checked_msg();
        f(cls, crate::sel!(alloc))
    }
}

/// `-(void)sel` — no arguments.
/// # Safety
/// See the module note: `sel` must be `-(void)` with no arguments on `recv`.
pub unsafe fn send_v(recv: Id, sel: Sel) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) = checked_msg();
        f(recv, sel);
    }
}

/// `-(void)sel:(id)a`.
/// # Safety
/// See the module note.
pub unsafe fn send_v_id(recv: Id, sel: Sel, a: Id) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
        f(recv, sel, a);
    }
}

/// `-(void)sel:(id)a other:(id)b`.
/// # Safety
/// See the module note.
pub unsafe fn send_v_id_id(recv: Id, sel: Sel, a: Id, b: Id) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Id) = checked_msg();
        f(recv, sel, a, b);
    }
}

/// `-(void)sel:(BOOL)a`.
/// # Safety
/// See the module note.
pub unsafe fn send_v_bool(recv: Id, sel: Sel, a: bool) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Bool) = checked_msg();
        f(recv, sel, Bool::new(a));
    }
}

/// `-(void)sel:(NSInteger)a`.
/// # Safety
/// See the module note.
pub unsafe fn send_v_isize(recv: Id, sel: Sel, a: isize) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, isize) = checked_msg();
        f(recv, sel, a);
    }
}

/// `-(void)sel:(NSUInteger)a`.
/// # Safety
/// See the module note.
pub unsafe fn send_v_usize(recv: Id, sel: Sel, a: usize) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, usize) = checked_msg();
        f(recv, sel, a);
    }
}

/// `-(void)sel:(NSSize)a`.
/// # Safety
/// See the module note.
pub unsafe fn send_v_size(recv: Id, sel: Sel, a: CGSize) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGSize) = checked_msg();
        f(recv, sel, a);
    }
}

/// `-(id)sel`.
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id(recv: Id, sel: Sel) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
        f(recv, sel)
    }
}

/// `-(id)sel:(id)a`.
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_id(recv: Id, sel: Sel, a: Id) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
        f(recv, sel, a)
    }
}

/// `-(id)sel:(id)a b:(SEL)b c:(id)c` — `NSMenuItem`'s designated initializer shape.
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_id_sel_id(recv: Id, sel: Sel, a: Id, b: Sel, c: Id) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Sel, Id) -> Id = checked_msg();
        f(recv, sel, a, b, c)
    }
}

/// `-(id)sel:(CGFloat)a`.
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_f64(recv: Id, sel: Sel, a: f64) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, f64) -> Id = checked_msg();
        f(recv, sel, a)
    }
}

/// `-(id)sel:(CGFloat)a b:(CGFloat)b` — `+[NSFont monospacedDigitSystemFontOfSize:weight:]`.
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_f64_f64(recv: Id, sel: Sel, a: f64, b: f64) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, f64, f64) -> Id = checked_msg();
        f(recv, sel, a, b)
    }
}

/// `-(id)sel:(NSRect)a` — `-[NSView initWithFrame:]` and friends.
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_rect(recv: Id, sel: Sel, a: CGRect) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect) -> Id = checked_msg();
        f(recv, sel, a)
    }
}

/// `-(BOOL)sel`.
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_bool(recv: Id, sel: Sel) -> bool {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> Bool = checked_msg();
        f(recv, sel).as_bool()
    }
}

/// `-(BOOL)sel:(SEL)a` — `-respondsToSelector:`.
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_bool_sel(recv: Id, sel: Sel, a: Sel) -> bool {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> Bool = checked_msg();
        f(recv, sel, a).as_bool()
    }
}

/// `-(BOOL)sel:(id)a` — `-isEqual:`, `-conformsToProtocol:`, ….
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_bool_id(recv: Id, sel: Sel, a: Id) -> bool {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Bool = checked_msg();
        f(recv, sel, a).as_bool()
    }
}

/// `-(NSInteger)sel`.
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_isize(recv: Id, sel: Sel) -> isize {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> isize = checked_msg();
        f(recv, sel)
    }
}

/// `-(NSUInteger)sel`.
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_usize(recv: Id, sel: Sel) -> usize {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
        f(recv, sel)
    }
}

/// `-(CGFloat)sel`.
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_f64(recv: Id, sel: Sel) -> f64 {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> f64 = checked_msg();
        f(recv, sel)
    }
}

/// `-(NSRect)sel`.
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
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> CGRect = checked_msg();
        f(recv, sel)
    }
}

/// `-(void)sel:(CGFloat)a` — `-[NSBezierPath setLineWidth:]`.
/// # Safety
/// See the module note.
pub unsafe fn send_v_f64(recv: Id, sel: Sel, a: f64) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, f64) = checked_msg();
        f(recv, sel, a);
    }
}

/// `-(void)sel:(NSPoint)a` — `-[NSBezierPath moveToPoint:]` / `lineToPoint:`.
/// # Safety
/// See the module note.
pub unsafe fn send_v_point(recv: Id, sel: Sel, a: CGPoint) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGPoint) = checked_msg();
        f(recv, sel, a);
    }
}

/// `-(void)sel:(NSRect)a` — `-[NSView setFrame:]`.
/// # Safety
/// See the module note.
pub unsafe fn send_v_rect(recv: Id, sel: Sel, a: CGRect) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect) = checked_msg();
        f(recv, sel, a);
    }
}

/// `-(void)sel:(id)a b:(id)b c:(id)c` — `+[NSMenu popUpContextMenu:withEvent:forView:]`.
/// # Safety
/// See the module note.
pub unsafe fn send_v_id_id_id(recv: Id, sel: Sel, a: Id, b: Id, c: Id) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Id, Id) = checked_msg();
        f(recv, sel, a, b, c);
    }
}

/// `-(void)sel:(id)a b:(NSInteger)b c:(id)c`.
/// # Safety
/// See the module note.
pub unsafe fn send_v_id_isize_id(recv: Id, sel: Sel, a: Id, b: isize, c: Id) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, isize, Id) = checked_msg();
        f(recv, sel, a, b, c);
    }
}

/// `-(NSPoint)sel` — `-[NSEvent locationInWindow]`.
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_point(recv: Id, sel: Sel) -> CGPoint {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> CGPoint = checked_msg();
        f(recv, sel)
    }
}

/// `-(NSPoint)sel:(NSPoint)a` — `-[NSWindow convertPointToScreen:]`.
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_point_point(recv: Id, sel: Sel, a: CGPoint) -> CGPoint {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGPoint) -> CGPoint = checked_msg();
        f(recv, sel, a)
    }
}

/// `-(NSPoint)sel:(NSPoint)a fromView:(id)b` — `-[NSView convertPoint:fromView:]`.
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_point_point_id(recv: Id, sel: Sel, a: CGPoint, b: Id) -> CGPoint {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGPoint, Id) -> CGPoint = checked_msg();
        f(recv, sel, a, b)
    }
}

/// `-(NSRect)sel:(NSRect)a fromView:(id)b` — `-[NSView convertRect:fromView:]`.
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect, Id) -> CGRect = checked_msg();
        f(recv, sel, a, b)
    }
}

/// `-(id)sel:(NSUInteger)a`.
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_usize(recv: Id, sel: Sel, a: usize) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = checked_msg();
        f(recv, sel, a)
    }
}

/// `-(id)sel:(NSRect)a xRadius:(CGFloat)b yRadius:(CGFloat)c`.
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_rect_f64_f64(recv: Id, sel: Sel, a: CGRect, b: f64, c: f64) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect, f64, f64) -> Id = checked_msg();
        f(recv, sel, a, b, c)
    }
}

/// `-(id)sel:(CGFloat)a b:(CGFloat)b c:(CGFloat)c d:(CGFloat)d`.
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_f64_f64_f64_f64(recv: Id, sel: Sel, a: f64, b: f64, c: f64, d: f64) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, f64, f64, f64, f64) -> Id = checked_msg();
        f(recv, sel, a, b, c, d)
    }
}

/// `-(id)sel:(NSRect)a options:(NSUInteger)b owner:(id)c userInfo:(id)d`.
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect, usize, Id, Id) -> Id = checked_msg();
        f(recv, sel, a, b, c, d)
    }
}

/// `-(id)sel:(id)a target:(id)b action:(SEL)c` — `+[NSButton buttonWithTitle:target:action:]`.
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_id_id_sel(recv: Id, sel: Sel, a: Id, b: Id, c: Sel) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Id, Sel) -> Id = checked_msg();
        f(recv, sel, a, b, c)
    }
}

/// `-(void)sel:(NSRect)a flag:(BOOL)b`.
/// # Safety
/// See the module note.
pub unsafe fn send_v_rect_bool(recv: Id, sel: Sel, a: CGRect, b: bool) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect, Bool) = checked_msg();
        f(recv, sel, a, Bool::new(b));
    }
}

/// `-(void)sel:(NSRect)a obj:(id)b`.
/// # Safety
/// See the module note.
pub unsafe fn send_v_rect_id(recv: Id, sel: Sel, a: CGRect, b: Id) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect, Id) = checked_msg();
        f(recv, sel, a, b);
    }
}

/// `-(void)sel:(id)a other:(NSInteger)b`.
/// # Safety
/// See the module note.
pub unsafe fn send_v_id_isize(recv: Id, sel: Sel, a: Id, b: isize) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, isize) = checked_msg();
        f(recv, sel, a, b);
    }
}

/// `-(void)sel:(id)a b:(id)b c:(NSUInteger)c d:(void *)d`.
/// # Safety
/// See the module note. `d` is not dereferenced here and must satisfy whatever the observing method assumes of it.
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
            checked_msg();
        f(recv, sel, a, b, c, d);
    }
}

/// `-(void)sel:(SEL)a withObject:(id)b afterDelay:(NSTimeInterval)c`.
/// # Safety
/// See the module note.
pub unsafe fn send_v_sel_id_f64(recv: Id, sel: Sel, a: Sel, b: Id, c: f64) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Sel, Id, f64) = checked_msg();
        f(recv, sel, a, b, c);
    }
}

/// `-(void)sel:(id)a selector:(SEL)b name:(id)c object:(id)d`.
/// # Safety
/// See the module note.
pub unsafe fn send_v_id_sel_id_id(recv: Id, sel: Sel, a: Id, b: Sel, c: Id, d: Id) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Sel, Id, Id) = checked_msg();
        f(recv, sel, a, b, c, d);
    }
}

/// `+(id)sel:(const id *)a count:(NSUInteger)b`.
/// # Safety
/// See the module note, including the ownership paragraph. `a` must point to at least `b` live object pointers, none of them nil.
#[must_use]
pub unsafe fn send_id_idptr_usize(recv: Id, sel: Sel, a: *const Id, b: usize) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, *const Id, usize) -> Id = checked_msg();
        f(recv, sel, a, b)
    }
}

/// `-(NSRect)sel:(NSRect)a`.
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect) -> CGRect = checked_msg();
        f(recv, sel, a)
    }
}

/// `-(NSInteger)sel:(NSUInteger)a` — `-[NSApplication requestUserAttention:]`.
/// # Safety
/// See the module note.
pub unsafe fn send_isize_usize(recv: Id, sel: Sel, a: usize) -> isize {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, usize) -> isize = checked_msg();
        f(recv, sel, a)
    }
}

/// `-(NSInteger)sel:(NSRect)a owner:(id)b userData:(void *)c assumeInside:(BOOL)d`.
/// # Safety
/// See the module note. `b` is not retained, so it must outlive the tracking rect or be removed first.
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
        ) -> isize = checked_msg();
        f(recv, sel, a, b, c, Bool::new(d))
    }
}

/// `-(unsigned short)sel`.
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_u16(recv: Id, sel: Sel) -> u16 {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> u16 = checked_msg();
        f(recv, sel)
    }
}

/// `-(BOOL)sel:(NSInteger)a`.
/// # Safety
/// See the module note.
pub unsafe fn send_bool_isize(recv: Id, sel: Sel, a: isize) -> bool {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, isize) -> Bool = checked_msg();
        f(recv, sel, a).as_bool()
    }
}

/// `-(void)sel:(id)a atStart:(BOOL)b`.
/// # Safety
/// See the module note.
pub unsafe fn send_v_id_bool(recv: Id, sel: Sel, a: Id, b: bool) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Bool) = checked_msg();
        f(recv, sel, a, Bool::new(b));
    }
}

/// `-(unsigned int)sel`.
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_u32(recv: Id, sel: Sel) -> u32 {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> u32 = checked_msg();
        f(recv, sel)
    }
}

/// `-(BOOL)sel:(Class)a`.
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_bool_cls(recv: Id, sel: Sel, a: ClassPtr) -> bool {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, ClassPtr) -> Bool = checked_msg();
        f(recv, sel, a).as_bool()
    }
}

/// `+(id)keyEventWithType:location:modifierFlags:timestamp:windowNumber:` `context:characters:charactersIgnoringModifiers:isARepeat:keyCode:`.
/// # Safety
/// See the module note, including the ownership paragraph — the result is +0 autoreleased, not +1.
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
        ) -> Id = checked_msg();
        f(recv, sel, a, b, c, d, e, f_, g, h, Bool::new(i), j)
    }
}

/// `+[NSEvent otherEventWithType:location:modifierFlags:timestamp:windowNumber: context:subtype:data1:data2:]`.
/// # Safety
/// See the module note, including the ownership paragraph — the result is +0 autoreleased, not +1.
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
        ) -> Id = checked_msg();
        f(recv, sel, a, b, c, d, e, f_, g, h, i)
    }
}

/// `-(float)sel` — a C `float`, not a `CGFloat`.
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_f32(recv: Id, sel: Sel) -> f32 {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> f32 = checked_msg();
        f(recv, sel)
    }
}

/// `-(id)sel:(SEL)a`.
/// # Safety
/// See the module note, including the ownership paragraph. The result follows the invoked method's convention; the `+…Cursor` accessors return +0.
#[must_use]
pub unsafe fn send_id_sel(recv: Id, sel: Sel, a: Sel) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> Id = checked_msg();
        f(recv, sel, a)
    }
}

/// `-(NSUInteger)sel:(NSUInteger)a`.
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_usize_usize(recv: Id, sel: Sel, a: usize) -> usize {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, usize) -> usize = checked_msg();
        f(recv, sel, a)
    }
}

/// `-(unsigned char *)sel` — an interior pointer into the receiver's storage.
/// # Safety
/// See the module note. The pointer is valid only while `recv` is alive and its buffer is not reallocated.
#[must_use]
pub unsafe fn send_charptr(recv: Id, sel: Sel) -> *mut core::ffi::c_uchar {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> *mut core::ffi::c_uchar = checked_msg();
        f(recv, sel)
    }
}

/// `-(id)sel:(const void *)a length:(NSUInteger)b`.
/// # Safety
/// See the module note, including the ownership paragraph — an initialiser, so the result is +1. `a` must point to at least `b` readable bytes for the call; they are copied.
#[must_use]
pub unsafe fn send_id_cptr_usize(recv: Id, sel: Sel, a: *const core::ffi::c_void, b: usize) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, *const core::ffi::c_void, usize) -> Id =
            checked_msg();
        f(recv, sel, a, b)
    }
}

/// `-[NSBitmapImageRep initWithBitmapDataPlanes:pixelsWide:pixelsHigh: bitsPerSample:samplesPerPixel:hasAlpha:isPlanar:colorSpaceName:bytesPerRow: bitsPerPixel:]`.
/// # Safety
/// See the module note, including the ownership paragraph — an initialiser, so the result is +1. `a` must be null or point to `spp` plane pointers; `h` must be a live `NSColorSpaceName` (an `NSString`).
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
        ) -> Id = checked_msg();
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
/// # Safety
/// See the module note, including the ownership paragraph — an initialiser, so the result is +1.
#[must_use]
pub unsafe fn send_id_size(recv: Id, sel: Sel, a: CGSize) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGSize) -> Id = checked_msg();
        f(recv, sel, a)
    }
}

/// `-(id)sel:(id)a hotSpot:(NSPoint)b`.
/// # Safety
/// See the module note, including the ownership paragraph — an initialiser, so the result is +1. `a` must be a live `NSImage`; the receiver retains it.
#[must_use]
pub unsafe fn send_id_id_point(recv: Id, sel: Sel, a: Id, b: CGPoint) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, CGPoint) -> Id = checked_msg();
        f(recv, sel, a, b)
    }
}

/// `-(id)sel:(id)a withInteger:(NSInteger)b`.
/// # Safety
/// See the module note, including the ownership paragraph — the result is +0 and borrowed. `a` must be a live object of the class the selector expects.
#[must_use]
pub unsafe fn send_id_id_isize(recv: Id, sel: Sel, a: Id, b: isize) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, isize) -> Id = checked_msg();
        f(recv, sel, a, b)
    }
}

/// `-(id)sel:(NSUInteger)a withObject:(id)b`.
/// # Safety
/// See the module note, including the ownership paragraph — the result is +0 and borrowed. `b` must be nil or a live object of the expected class.
#[must_use]
pub unsafe fn send_id_usize_id(recv: Id, sel: Sel, a: usize, b: Id) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, usize, Id) -> Id = checked_msg();
        f(recv, sel, a, b)
    }
}

/// `-(void)sel:(void *)a length:(NSUInteger)b`.
/// # Safety
/// See the module note. `a` must point at `b` writable bytes, and `b` must not exceed the receiver's length (`NSData` raises rather than truncating).
pub unsafe fn send_v_ptr_usize(recv: Id, sel: Sel, a: *mut core::ffi::c_void, b: usize) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, *mut core::ffi::c_void, usize) = checked_msg();
        f(recv, sel, a, b);
    }
}

/// `-(id)sel:(NSRect)a styleMask:(NSUInteger)b backing:(NSUInteger)c defer:(BOOL)d`.
/// # Safety
/// See the module note, including the ownership paragraph: as an initializer it consumes the receiver's +1 from `+alloc` and returns +1 (or nil, having released it).
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
            checked_msg();
        f(recv, sel, a, b, c, Bool::new(d))
    }
}

/// `-(id)sel:(id)a event:(id)b source:(id)c`.
/// # Safety
/// See the module note, including the ownership paragraph — the result is +0 and borrowed.
#[must_use]
pub unsafe fn send_id_id_id_id(recv: Id, sel: Sel, a: Id, b: Id, c: Id) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Id, Id) -> Id = checked_msg();
        f(recv, sel, a, b, c)
    }
}

/// `-(void)sel:(SEL)a`.
/// # Safety
/// See the module note. AppKit stores the selector and sends it to the target later; that the target answers it is the caller's obligation.
pub unsafe fn send_v_sel(recv: Id, sel: Sel, a: Sel) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Sel) = checked_msg();
        f(recv, sel, a);
    }
}

/// `-(NSSize)sel`.
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_size(recv: Id, sel: Sel) -> CGSize {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> CGSize = checked_msg();
        f(recv, sel)
    }
}

/// `-(NSSize)sel:(NSRect)a`.
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_size_rect(recv: Id, sel: Sel, a: CGRect) -> CGSize {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect) -> CGSize = checked_msg();
        f(recv, sel, a)
    }
}

/// `-(int)sel`.
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_i32(recv: Id, sel: Sel) -> i32 {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> i32 = checked_msg();
        f(recv, sel)
    }
}

/// `-(BOOL)sel:(id)a forType:(id)b`.
/// # Safety
/// See the module note.
pub unsafe fn send_bool_id_id(recv: Id, sel: Sel, a: Id, b: Id) -> bool {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Id) -> Bool = checked_msg();
        f(recv, sel, a, b).as_bool()
    }
}

/// `-(id)sel:(id)a isDirectory:(BOOL)b`.
/// # Safety
/// See the module note, including the ownership paragraph — the result is +0 and borrowed.
#[must_use]
pub unsafe fn send_id_id_bool(recv: Id, sel: Sel, a: Id, b: bool) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Bool) -> Id = checked_msg();
        f(recv, sel, a, Bool::new(b))
    }
}

/// `-(BOOL)sel:(NSUInteger)a`.
/// # Safety
/// See the module note.
pub unsafe fn send_bool_usize(recv: Id, sel: Sel, a: usize) -> bool {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Bool = checked_msg();
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
        assert!(!same("q@:Q", "Q@:Q"));
        assert!(!same("f@:", "d@:"));
        assert!(!same("S@:", "Q@:"));
        assert!(!same("v@:{CGSize=dd}", "v@:{CGPoint=dd}"));
        assert!(!same("v@:@", "v@:@@"), "the arity counts");
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
        assert!(swift_override_of_the_same_width(
            "Swift.__StringStorage",
            "q@:",
            "Q@:"
        ));
        assert!(swift_override_of_the_same_width(
            "Swift.__StringStorage",
            "v@:^S{?=qq}",
            "v@:^S{_NSRange=QQ}"
        ));
        assert!(swift_override_of_the_same_width(
            "Swift.__StringStorage",
            "c@:*qQ",
            "B@:*QQ"
        ));
        assert!(!swift_override_of_the_same_width(
            "Swift.__StringStorage",
            "v@:^S{?=qi}",
            "v@:^S{_NSRange=QQ}"
        ));
        assert!(!swift_override_of_the_same_width(
            "Swift.__StringStorage",
            "v@:^S{?=dd}",
            "v@:^S{_NSRange=QQ}"
        ));
        assert!(!swift_override_of_the_same_width(
            "NSWindow", "q@:Q", "Q@:Q"
        ));
        assert!(!swift_override_of_the_same_width(swift, "v@:s", "v@:S"));
        assert!(!swift_override_of_the_same_width(swift, "q@:", "I@:"));
    }

    #[test]
    fn a_prototype_is_checked_without_a_send() {
        let string = crate::class(c"NSString");
        // SAFETY: live classes; nothing below sends a message.
        unsafe {
            prototype_check::<unsafe extern "C-unwind" fn(Id, Sel) -> usize>(
                string,
                crate::sel!(length),
            )
            .expect("-[NSString length] is NSUInteger");
            let wrong = prototype_check::<unsafe extern "C-unwind" fn(Id, Sel) -> isize>(
                string,
                crate::sel!(length),
            )
            .expect_err("a signed -length");
            assert!(
                wrong.contains("`Q@:`") && wrong.contains("`q@:`"),
                "{wrong}"
            );
            let meta = crate::class_of(string.as_id());
            prototype_check::<unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id>(
                meta,
                crate::sel!(string),
            )
            .expect("+[NSString string] is on the metaclass");
            assert!(
                prototype_check::<unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id>(
                    string,
                    crate::sel!(string),
                )
                .is_err(),
                "+string is no instance method"
            );
            assert!(
                prototype_check::<unsafe extern "C-unwind" fn(Id, Sel)>(
                    string,
                    crate::sel!(atermNoSuchSelector),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn a_block_prototype_is_checked_against_its_recorded_signature() {
        assert_eq!(
            block_signatures(
                "v40@0:8@\"UNUserNotificationCenter\"16@\"UNNotification\"24@?<v@?Q>32"
            ),
            ["v@?Q"]
        );
        assert_eq!(
            block_signatures("v32@0:8@?<v@?@\"NSError\">16@?<v@?@?<v@?B>>24"),
            ["v@?@\"NSError\"", "v@?@?<v@?B>"]
        );
        assert!(
            block_signatures("v24@0:8@?16").is_empty(),
            "a bare block records none"
        );
        assert!(
            same("v@:@?<v@?Q>@", "v@:@?@"),
            "a recorded signature is one pointer"
        );
        block_check::<fn(usize)>("v@?Q").expect("NSUInteger options");
        block_check::<fn()>("v@?").expect("no arguments");
        block_check::<fn(Bool, Id)>(&format!("v@?{}@\"NSError\"", Bool::ENCODING))
            .expect("BOOL granted, NSError *error");
        block_check::<fn(Id)>("v@?@\"UNNotificationSettings\"").expect("one object");
        for (case, result) in [
            ("a signed options word", block_check::<fn(isize)>("v@?Q")),
            ("an argument too many", block_check::<fn(Id)>("v@?")),
            ("an argument too few", block_check::<fn()>("v@?@")),
            (
                "a scalar for an object's place",
                block_check::<fn(i32)>("v@?@"),
            ),
            ("a return", block_check::<fn() -> usize>("v@?")),
        ] {
            assert!(result.is_err(), "{case}");
        }
    }

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

    #[test]
    #[cfg(debug_assertions)]
    fn a_reused_checked_cast_is_checked_per_receiver_and_selector() {
        let s = crate::ns_string("four").expect("NSString");
        // SAFETY: `-length` and `-hash` are `-(NSUInteger)` on a live NSString; `+hash` is `+(NSUInteger)` on the NSString class.
        let (len, _, _) = unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
            let g: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> usize = checked_msg();
            (
                f(s.id(), crate::sel!(length)),
                f(s.id(), crate::sel!(hash)),
                g(crate::class(c"NSString"), crate::sel!(hash)),
            )
        };
        assert_eq!(len, 4);
        for attempt in 0..2 {
            let wrong = std::panic::catch_unwind(|| {
                // SAFETY: never sent: `-description` is `@@:`, and the check refuses an `NSUInteger` return first.
                unsafe {
                    let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
                    f(s.id(), crate::sel!(description))
                }
            });
            assert!(
                wrong.is_err(),
                "attempt {attempt}: a wrong prototype was sent"
            );
        }
        let wrong_class = std::panic::catch_unwind(|| {
            // SAFETY: never sent: the check refuses it first.
            unsafe {
                let f: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Bool = checked_msg();
                f(crate::class(c"NSString"), crate::sel!(string))
            }
        });
        assert!(
            wrong_class.is_err(),
            "a class method's prototype went unchecked"
        );
    }
}
