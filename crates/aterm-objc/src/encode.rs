// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Objective-C type encodings.
//!
//! `class_addMethod` takes a `types` string describing the method's C
//! signature. Direct sends never read it, but `NSMethodSignature` does, and
//! AppKit uses that on every reflective path (`NSInvocation`, forwarding,
//! accessibility, delegate arity checks).
//!
//! Encodings here are the offset-free form (`"v@:@"`, not `"v24@0:8@16"`):
//! `NSMethodSignature` recomputes offsets from the type letters and ignores
//! any that are written.

use std::ffi::c_void;
use std::fmt;

/// The Objective-C type encoding of a value that can cross a method boundary.
///
/// # Safety
/// `ENCODING` must be the encoding the C ABI actually uses for `Self`. A wrong
/// string does not affect direct sends (typed by [`crate::msg`]) but makes
/// every reflective path read arguments at the wrong widths.
pub unsafe trait Encode {
    /// The encoding string, e.g. `"@"` for an object or `"d"` for a `CGFloat`.
    const ENCODING: &'static str;

    /// Whether `Self` contains, anywhere in its layout tree, a field at an
    /// offset that is not a multiple of that field's own alignment.
    ///
    /// Transitive: a wrapper whose only field is a packed struct is still
    /// MEMORY-classified on System V x86-64, because classification walks the
    /// whole layout. From `clang -arch x86_64 -O1 -S`, with
    /// `struct __attribute__((packed)) Packed9 { char c; long q; }`:
    ///
    /// ```text
    /// struct Wrap    { struct Packed9 inner; }              9  1  _objc_msgSend_stret
    /// struct WrapPad { struct Packed9 inner; char pad[7]; } 16  1  _objc_msgSend_stret
    /// struct ArrOne  { struct Packed9 a[1]; }               9  1  _objc_msgSend_stret
    /// struct NineChars { char c[9]; }                       9  1  _objc_msgSend
    /// ```
    ///
    /// Not an encoding (`@encode` does not record packing); it lives here
    /// because this trait is where a type declares its C ABI, and
    /// [`crate::returns_indirectly`] needs it: on `x86_64-apple-darwin` a type
    /// with an unaligned field is MEMORY at any size.
    ///
    /// Defaults to `false`; nothing in Foundation, AppKit, CoreGraphics or this
    /// crate is packed. A packed type sets it by hand:
    ///
    /// ```
    /// # use aterm_objc::{Encode, returns_indirectly};
    /// #[repr(C, packed)]
    /// struct Packed {
    ///     c: i8,
    ///     q: i64,
    /// }
    /// // SAFETY: `{Packed=cq}` is the layout; the `q` sits at offset 1, so
    /// // System V x86-64 classification is MEMORY regardless of the 9-byte size.
    /// unsafe impl Encode for Packed {
    ///     const ENCODING: &'static str = "{Packed=cq}";
    ///     const HAS_UNALIGNED_FIELDS: bool = true;
    /// }
    /// assert_eq!(size_of::<Packed>(), 9);
    /// assert_eq!(returns_indirectly::<Packed>(), cfg!(target_arch = "x86_64"));
    /// ```
    ///
    /// A wrapper inherits it; prefer `<Field as Encode>::HAS_UNALIGNED_FIELDS`
    /// over a hand-typed `true`:
    ///
    /// ```
    /// # use aterm_objc::{Encode, returns_indirectly};
    /// #[repr(C, packed)]
    /// struct Packed { c: i8, q: i64 }
    /// // SAFETY: `{Packed=cq}`; the `q` sits at offset 1.
    /// unsafe impl Encode for Packed {
    ///     const ENCODING: &'static str = "{Packed=cq}";
    ///     const HAS_UNALIGNED_FIELDS: bool = true;
    /// }
    /// #[repr(C)]
    /// struct Wrap { inner: Packed }
    /// // ONE field, at offset 0, alignment 1: only the field's own flag
    /// // knows its `q` is misaligned.
    /// // SAFETY: the misalignment is `Packed`'s and `Wrap` inherits it.
    /// unsafe impl Encode for Wrap {
    ///     const ENCODING: &'static str = "{Wrap={Packed=cq}}";
    ///     const HAS_UNALIGNED_FIELDS: bool = <Packed as Encode>::HAS_UNALIGNED_FIELDS;
    /// }
    /// assert_eq!(size_of::<Wrap>(), 9);
    /// assert_eq!(align_of::<Wrap>(), 1);
    /// assert_eq!(returns_indirectly::<Wrap>(), cfg!(target_arch = "x86_64"));
    /// ```
    ///
    /// # Safety
    /// `false` for a type that misaligns a field anywhere in its layout routes
    /// an x86_64 send to the wrong entry point, shifting every argument by one
    /// register.
    const HAS_UNALIGNED_FIELDS: bool = false;
}

macro_rules! prim {
    ($($t:ty => $e:literal),* $(,)?) => {$(
        // SAFETY: the letters are the runtime's documented encodings and match
        // the Rust type's C ABI on every 64-bit Apple target.
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

/// `@encode(BOOL)` for the target being compiled:
///
/// ```text
/// clang -arch arm64  : @encode(BOOL) = "B", __OBJC_BOOL_IS_BOOL = 1
/// clang -arch x86_64 : @encode(BOOL) = "c", __OBJC_BOOL_IS_BOOL = 0
/// ```
///
/// Both arms are live: x86_64 is the compat slice of the universal binary.
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

/// The Objective-C `BOOL`, at the width, values and encoding this target uses.
///
/// On `aarch64-apple-darwin` `BOOL` is `_Bool`; on `x86_64-apple-darwin` it is
/// `signed char`, and the sender passes the byte unnormalised:
///
/// ```text
/// arm64  : cmp w1, #0 ; cset w2, ne     <- normalised to 0/1 (it is `_Bool`)
/// x86_64 : movsbl %sil, %edx            <- the byte is passed AS IS
/// ```
///
/// So a declared method can receive a `BOOL` of `3`, and reading that as a Rust
/// `bool` is undefined behaviour. Every declared `BOOL` argument and return
/// uses this type, `bool` has no [`Encode`] impl, and conversion is explicit.
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

// SAFETY: `Bool` is `#[repr(transparent)]` over this target's `BOOL` C type
// (`_Bool` on aarch64, `signed char` elsewhere), and `BOOL_ENCODING` is its
// `@encode`. `bool_encoding_matches_what_foundation_itself_emits` re-checks it
// against a Foundation method signature at run time.
unsafe impl Encode for Bool {
    const ENCODING: &'static str = BOOL_ENCODING;
}

// The pointer-shaped runtime types are distinct newtypes so each carries the
// letter clang emits:
//
// ```text
// @encode(id)           = @      @encode(Class)        = #
// @encode(Protocol *)   = @      @encode(SEL)          = :
// @encode(void *)       = ^v     @encode(const void *) = r^v
// ```
//
// SAFETY: object pointers encode as `"@"`, and `Id` is `#[repr(transparent)]`
// over exactly the pointer the runtime passes.
unsafe impl Encode for crate::runtime::Id {
    const ENCODING: &'static str = "@";
}
// SAFETY: a class object encodes as `"#"`, not `"@"` (`- (void)
// setDelegateClass:(Class)c` registers `v24@0:8#16`).
unsafe impl Encode for crate::runtime::ClassPtr {
    const ENCODING: &'static str = "#";
}
// SAFETY: `@encode(Protocol *)` is `"@"` (measured; unlike `Class`).
unsafe impl Encode for crate::runtime::ProtocolPtr {
    const ENCODING: &'static str = "@";
}
// SAFETY: selectors encode as `":"`; `Sel` is a newtype so this can differ from
// the `void *` impls below.
unsafe impl Encode for crate::runtime::Sel {
    const ENCODING: &'static str = ":";
}
// A bare `void *`: an opaque caller-owned context pointer (KVO's `context:`),
// not an object. Registering it as `"@"` would invite `NSInvocation` and
// forwarding to retain it.
// SAFETY: `void *` encodes as `"^v"` — `^` is "pointer to", `v` is `void`.
unsafe impl Encode for *mut c_void {
    const ENCODING: &'static str = "^v";
}
// clang emits `r^v`; the `r` (const qualifier) is optional in a method type
// string and `NSMethodSignature` skips it, so the short `"^v"` is registered.
// SAFETY: as above.
unsafe impl Encode for *const c_void {
    const ENCODING: &'static str = "^v";
}
// `char *` — what `-[NSString UTF8String]` returns.
// SAFETY: the runtime spells a C string `"*"`, not `"^c"`.
unsafe impl Encode for *const std::ffi::c_char {
    const ENCODING: &'static str = "*";
}
// `unsigned char *` — what `-[NSBitmapImageRep bitmapData]` returns. clang
// encodes `char *`, `signed char *` and `unsigned char *` all as `*`, and the
// live method reads `*16@0:8`; `^C` would be a string the runtime never emits.
// SAFETY: measured with clang on both arches.
unsafe impl Encode for *mut std::ffi::c_uchar {
    const ENCODING: &'static str = "*";
}

/// `@encode(BOOL *)` for the target being compiled. Not `"^"` plus
/// [`BOOL_ENCODING`] on x86_64, and not `*` either:
///
/// ```text
/// clang -arch arm64  : @encode(BOOL*) = "^B"   (BOOL is _Bool)
/// clang -arch x86_64 : @encode(BOOL*) = "^c"   (BOOL is signed char, but
///                                               clang encodes the BOOL
///                                               typedef itself, never the
///                                               char* it aliases)
/// ```
///
/// Foundation's compiled methods agree:
/// `-[NSFileManager fileExistsAtPath:isDirectory:]` is `c32@0:8@16^c24` on
/// x86_64. `tests/adversary_w2d.rs` compares that argument with this constant.
#[cfg(target_arch = "aarch64")]
const BOOL_PTR_ENCODING: &str = "^B";
/// See the `aarch64` arm.
#[cfg(not(target_arch = "aarch64"))]
const BOOL_PTR_ENCODING: &str = "^c";

// `BOOL *` — the `stop` out-parameter of Foundation's `enumerate…UsingBlock:`.
// `*mut bool` would break [`Bool`]'s rule through a pointer (x86_64 writes an
// arbitrary `signed char`), and neither `bool` nor `*mut bool` has an `Encode`
// impl, so this is the only spelling `RcBlock::newN` accepts.
// SAFETY: measured on both arms — see [`BOOL_PTR_ENCODING`].
unsafe impl Encode for *mut Bool {
    const ENCODING: &'static str = BOOL_PTR_ENCODING;
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

// SAFETY: these are Foundation's struct encodings for the same layouts;
// `#[repr(C)]` with two `f64` fields matches `CGPoint`/`CGSize`, and `CGRect`
// is the two in order. `CGFloat` is `double` on 64-bit.
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

/// An `NSRange` — a location/length pair over `NSUInteger`, needed by the
/// `NSTextInputClient` methods (`selectedRange`, `markedRange`,
/// `firstRectForCharacterRange:actualRange:`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NSRange {
    /// `NSNotFound` (`NSIntegerMax`) when there is no range.
    pub location: usize,
    /// Length in UTF-16 code units, which is what `NSTextInputClient` counts.
    pub length: usize,
}

// SAFETY: `@encode(NSRange)` is `{_NSRange=QQ}` (the tag is `_NSRange`; `NSRange`
// is a typedef), and `Q` is `usize` on 64-bit Apple targets. `#[repr(C)]` with
// two `usize` fields matches the C layout.
unsafe impl Encode for NSRange {
    const ENCODING: &'static str = "{_NSRange=QQ}";
}

// `NSRangePointer` (`NSRange *`), the out-parameter of
// `firstRectForCharacterRange:actualRange:`, which clang registers as
//
// ```text
// {CGRect={CGPoint=dd}{CGSize=dd}}40@0:8{_NSRange=QQ}16^{_NSRange=QQ}32
// ```
//
// A pointer to a struct encodes as `^` plus the pointee's encoding. Spelled per
// type because stable Rust cannot concatenate associated consts; another
// pointee needs a `#[repr(transparent)]` pointer newtype and its own impl.
// SAFETY: `^` is "pointer to" and `{_NSRange=QQ}` is the pointee's encoding.
unsafe impl Encode for *mut NSRange {
    const ENCODING: &'static str = "^{_NSRange=QQ}";
}
// SAFETY: as above; the `const` qualifier `r` is omitted for the reason given
// on `*const c_void`.
unsafe impl Encode for *const NSRange {
    const ENCODING: &'static str = "^{_NSRange=QQ}";
}

/// The offset-free form of a compiler-emitted method type encoding.
///
/// clang registers `"v24@0:8@16"` for `-(void)foo:(id)a`; this crate registers
/// `"v@:@"`. Comparing a declared row with its protocol or superclass needs one
/// normalised, and the short form loses nothing.
///
/// ```
/// # use aterm_objc::strip_method_offsets;
/// // `-[NSWindowDelegate windowDidResize:]`, as the runtime reports it.
/// assert_eq!(strip_method_offsets("v24@0:8@16"), "v@:@");
/// // `-[NSDraggingDestination draggingEntered:]` — an NSUInteger return.
/// assert_eq!(strip_method_offsets("Q24@0:8@16"), "Q@:@");
/// // The struct-returning row, offsets and all.
/// assert_eq!(
///     strip_method_offsets(
///         "{CGRect={CGPoint=dd}{CGSize=dd}}40@0:8{_NSRange=QQ}16^{_NSRange=QQ}32"
///     ),
///     "{CGRect={CGPoint=dd}{CGSize=dd}}@:{_NSRange=QQ}^{_NSRange=QQ}"
/// );
/// ```
///
/// Offsets appear only at the top level, after a complete type, so digits are
/// stripped only at nesting depth zero and outside quotes. That keeps array
/// counts (`[16c]`), bitfield widths (`b4b1b24`), and digits in struct, union,
/// template or quoted class names (`CATransform3D`, `MTLHashMask<4>`);
/// `tests/encode_offsets.rs` checks these against the live runtime.
///
/// ```
/// # use aterm_objc::strip_method_offsets;
/// // An array count.
/// assert_eq!(strip_method_offsets("v20@0:8[4i]16"), "v@:[4i]");
/// // A bitfield width — `NSDecimal`, off `-[NSString decimalValue]`.
/// assert_eq!(
///     strip_method_offsets("{?=b8b4b1b1b18[8S]}16@0:8"),
///     "{?=b8b4b1b1b18[8S]}@:"
/// );
/// // A digit in a struct tag.
/// assert_eq!(
///     strip_method_offsets("v144@0:8{CATransform3D=dddddddddddddddd}16"),
///     "v@:{CATransform3D=dddddddddddddddd}"
/// );
/// // A digit inside a QUOTED class name, which clang emits for a typed
/// // object argument.
/// assert_eq!(
///     strip_method_offsets("v24@0:8@\"NSISO8601DateFormatter\"16"),
///     "v@:@\"NSISO8601DateFormatter\""
/// );
/// ```
///
/// A `const` qualifier (`r`) is a letter and is not stripped, so `"r^v"` versus
/// `"^v"` remains a reported difference.
#[must_use]
pub fn strip_method_offsets(types: &str) -> String {
    let mut out = String::with_capacity(types.len());
    // One depth counter for all bracket kinds: only "inside anything" matters.
    let mut depth = 0_usize;
    let mut in_quotes = false;
    for c in types.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                out.push(c);
            }
            '{' | '(' | '[' if !in_quotes => {
                depth += 1;
                out.push(c);
            }
            '}' | ')' | ']' if !in_quotes => {
                depth = depth.saturating_sub(1);
                out.push(c);
            }
            '0'..='9' if depth == 0 && !in_quotes => {}
            _ => out.push(c),
        }
    }
    out
}

/// Build a method type-encoding string: return type, then the implicit
/// `self`/`_cmd` pair, then the explicit arguments. A `String` because
/// associated consts cannot be concatenated; it runs once per method at
/// registration.
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
