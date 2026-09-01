// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Objective-C type encodings.
//!
//! `class_addMethod` takes a `types` string describing the method's C
//! signature. `metal/ffi.rs` never needed one — it only ever SENT messages, and
//! a send carries no encoding. Defining a method does: the runtime hands the
//! string to `NSMethodSignature`, and AppKit reaches for that on every path
//! that does not send the selector directly (`NSInvocation`, key-value
//! forwarding, accessibility, and the `respondsToSelector:`-plus-`methodSignature
//! ForSelector:` dance a delegate protocol uses when the caller wants to inspect
//! arity before invoking).
//!
//! The encodings here are the OFFSET-FREE form (`"v@:@"`, not `"v24@0:8@16"`).
//! Compiler-emitted encodings carry byte offsets; `NSMethodSignature` recomputes
//! them from the type letters and ignores what is written, which is why the
//! short form is both accepted and what every runtime-declared class uses.

use std::ffi::c_void;
use std::fmt;

/// The Objective-C type encoding of a value that can cross a method boundary.
///
/// # Safety
/// `ENCODING` must be the encoding the C ABI actually uses for `Self`. A wrong
/// string does not corrupt a direct send (those are typed by [`crate::msg`]),
/// but it makes every reflective path — `NSInvocation`, forwarding,
/// accessibility — read the arguments at the wrong widths.
pub unsafe trait Encode {
    /// The encoding string, e.g. `"@"` for an object or `"d"` for a `CGFloat`.
    const ENCODING: &'static str;
}

macro_rules! prim {
    ($($t:ty => $e:literal),* $(,)?) => {$(
        // SAFETY: the encoding letters are from the Objective-C runtime's
        // documented table and match the Rust type's C ABI on every Apple
        // 64-bit target (the only targets this crate compiles for).
        unsafe impl Encode for $t { const ENCODING: &'static str = $e; }
    )*};
}

prim! {
    () => "v",
    i8 => "c",
    u8 => "C",
    i16 => "s",
    u16 => "S",
    i32 => "i",
    u32 => "I",
    i64 => "q",
    u64 => "Q",
    isize => "q",
    usize => "Q",
    f32 => "f",
    f64 => "d",
}

/// `@encode(BOOL)` for the target being compiled.
///
/// MEASURED with clang on this box rather than read off a table, because the
/// claim that used to sit here — "`BOOL` on every 64-bit Apple platform is C99
/// `_Bool`, encoded `\"B\"`" — is FALSE, and false on a target this repo
/// SHIPS:
///
/// ```text
/// clang -arch arm64  : @encode(BOOL) = "B", __OBJC_BOOL_IS_BOOL = 1
/// clang -arch x86_64 : @encode(BOOL) = "c", __OBJC_BOOL_IS_BOOL = 0
/// ```
///
/// `x86_64-apple-darwin` is the compat slice `lipo`d into aterm's universal
/// binary (see `Cargo.toml`'s six-cell note), so both arms are live code.
#[cfg(target_arch = "aarch64")]
const BOOL_ENCODING: &str = "B";
/// See the `aarch64` arm.
#[cfg(not(target_arch = "aarch64"))]
const BOOL_ENCODING: &str = "c";

/// The C type `BOOL` widens to on this target.
#[cfg(target_arch = "aarch64")]
type BoolRepr = bool;
/// See the `aarch64` arm: `signed char` everywhere else Apple ships.
#[cfg(not(target_arch = "aarch64"))]
type BoolRepr = i8;

/// The Objective-C `BOOL`, at the width, values and encoding THIS target uses.
///
/// # Why Rust's `bool` is not this type
///
/// On `aarch64-apple-darwin` `BOOL` *is* C99 `_Bool`, so `bool` would do. On
/// `x86_64-apple-darwin` it is `signed char`: 256 representable values, of
/// which only `0` and `1` are valid `bool` bit patterns. That is not a
/// theoretical difference — the SENDER's codegen differs, measured on
/// `[x setFlag:(BOOL)n]` for an arbitrary `int n`:
///
/// ```text
/// arm64  : cmp w1, #0 ; cset w2, ne     <- normalised to 0/1 (it is `_Bool`)
/// x86_64 : movsbl %sil, %edx            <- the byte is passed AS IS
/// ```
///
/// So on the compat slice a caller really can hand a declared method a `BOOL`
/// of `3`, and materialising that as a Rust `bool` is undefined behaviour, not
/// a wrong answer. Every declared `BOOL` argument and return therefore uses
/// this type, `bool` has no [`Encode`] impl at all, and the conversion is
/// explicit at one place per method body.
///
/// This is objc2's `runtime::Bool`, reached by the same reasoning; the crate
/// being retired was right about this and W1 was wrong.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Bool(BoolRepr);

#[cfg(target_arch = "aarch64")]
impl Bool {
    /// Objective-C `NO`.
    pub const NO: Self = Self(false);
    /// Objective-C `YES`.
    pub const YES: Self = Self(true);

    /// A `BOOL` from a Rust `bool`. Always exact: `bool` is a subset.
    #[inline]
    #[must_use]
    pub const fn new(value: bool) -> Self {
        Self(value)
    }

    /// The Rust `bool` this `BOOL` means.
    #[inline]
    #[must_use]
    pub const fn as_bool(self) -> bool {
        self.0
    }
}

#[cfg(not(target_arch = "aarch64"))]
impl Bool {
    /// Objective-C `NO`.
    pub const NO: Self = Self(0);
    /// Objective-C `YES`.
    pub const YES: Self = Self(1);

    /// A `BOOL` from a Rust `bool`.
    #[inline]
    #[must_use]
    pub const fn new(value: bool) -> Self {
        Self(value as BoolRepr)
    }

    /// The Rust `bool` this `BOOL` means — `!= 0`, which is C's own rule and
    /// the reason this crate never transmutes the byte.
    #[inline]
    #[must_use]
    pub const fn as_bool(self) -> bool {
        self.0 != 0
    }
}

impl From<bool> for Bool {
    fn from(value: bool) -> Self {
        Self::new(value)
    }
}

impl From<Bool> for bool {
    fn from(value: Bool) -> Self {
        value.as_bool()
    }
}

impl fmt::Debug for Bool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.as_bool() { "YES" } else { "NO" })
    }
}

// SAFETY: `Bool` is `#[repr(transparent)]` over exactly the C type this
// target's `BOOL` is (`_Bool` on aarch64, `signed char` elsewhere), and
// `BOOL_ENCODING` is that type's `@encode`, measured with clang on both. The
// runtime test `bool_encoding_matches_what_foundation_itself_emits` re-checks
// it against a compiler-emitted Foundation method signature at run time.
unsafe impl Encode for Bool {
    const ENCODING: &'static str = BOOL_ENCODING;
}

// `Id` is `*mut c_void`. In THIS crate a raw void pointer in a method position
// always means `id` — see the type alias's docs. Nothing in aterm's 33 methods
// or winit's 71 passes a non-object `void *`.
// SAFETY: object pointers encode as `"@"`.
unsafe impl Encode for *mut c_void {
    const ENCODING: &'static str = "@";
}
// `Sel` is `*const c_void` — the `_cmd` slot and `doCommandBySelector:`.
// SAFETY: selectors encode as `":"`.
unsafe impl Encode for *const c_void {
    const ENCODING: &'static str = ":";
}

/// A `CGPoint` / `NSPoint`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

/// A `CGSize` / `NSSize`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

/// A `CGRect` / `NSRect` — the argument `drawRect:` takes and `bounds` returns.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

// SAFETY: these are the struct encodings Foundation emits for the same layouts;
// `#[repr(C)]` with two `f64` fields matches `CGPoint`/`CGSize` exactly, and
// `CGRect` is the two nested in order. `CGFloat` is `double` on 64-bit.
unsafe impl Encode for CGPoint {
    const ENCODING: &'static str = "{CGPoint=dd}";
}
// SAFETY: as above.
unsafe impl Encode for CGSize {
    const ENCODING: &'static str = "{CGSize=dd}";
}
// SAFETY: as above.
unsafe impl Encode for CGRect {
    const ENCODING: &'static str = "{CGRect={CGPoint=dd}{CGSize=dd}}";
}

/// Build a method type-encoding string: return type, then the implicit
/// `self`/`_cmd` pair, then the explicit arguments.
///
/// Produces a `String` rather than a `&'static str` because `concat!` only
/// takes literals and an associated `const` is not one. The cost is irrelevant:
/// this runs once per method at class-registration time, never on a send.
///
/// ```
/// # use aterm_objc::{method_encoding, Bool, CGRect, Encode, Id, Sel};
/// // `- (void)reduceMotionDidChange:(id)note`
/// assert_eq!(method_encoding!(() ; Id), "v@:@");
/// // `- (BOOL)validateMenuItem:(id)item` — `BOOL` is "B" on arm64 and "c" on
/// // the x86_64 compat slice, so the expectation is written from the type.
/// assert_eq!(method_encoding!(Bool ; Id), format!("{}@:@", Bool::ENCODING));
/// // `- (void)drawRect:(NSRect)r`
/// assert_eq!(method_encoding!(() ; CGRect), "v@:{CGRect={CGPoint=dd}{CGSize=dd}}");
/// // no explicit arguments: `- (void)updateTrackingAreas`
/// assert_eq!(method_encoding!(()), "v@:");
/// // TWO arguments, and three: one semicolon, then a comma-separated list.
/// // `- (BOOL)control:(id)c textView:(id)tv doCommandBySelector:(SEL)s`
/// assert_eq!(
///     method_encoding!(Bool ; Id, Id, Sel),
///     format!("{}@:@@:", Bool::ENCODING)
/// );
/// assert_eq!(method_encoding!(Id ; Id, Id, Bool), format!("@@:@@{}", Bool::ENCODING));
/// ```
#[macro_export]
macro_rules! method_encoding {
    ($ret:ty $(; $($arg:ty),* $(,)?)?) => {{
        let mut __enc = ::std::string::String::new();
        __enc.push_str(<$ret as $crate::Encode>::ENCODING);
        __enc.push_str("@:");
        $($( __enc.push_str(<$arg as $crate::Encode>::ENCODING); )*)?
        __enc
    }};
}
