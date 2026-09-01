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

    /// Whether `Self` CONTAINS, anywhere in its layout tree, a field at an
    /// offset that is not a multiple of that field's own alignment.
    ///
    /// # The rule is TRANSITIVE, and saying so is the whole of the fix
    ///
    /// This used to read "whether `Self` places any field at an offset that is
    /// not a multiple of that field's own alignment", which is a rule about
    /// `Self`'s OWN fields and nothing deeper. Applied literally it answers
    /// `false` for a wrapper that has exactly one field, at offset 0, whose own
    /// alignment is 1 — and such a wrapper is still MEMORY-classified on
    /// System V x86-64, because the classification walks the whole layout tree.
    /// CODEGEN-PROVED with `clang -arch x86_64 -O1 -S` (this box cannot execute
    /// that slice), three shapes, all built from the same 9-byte packed struct
    /// `struct __attribute__((packed)) Packed9 { char c; long q; }`:
    ///
    /// ```text
    /// struct Wrap    { struct Packed9 inner; }              9  1  _objc_msgSend_stret
    /// struct WrapPad { struct Packed9 inner; char pad[7]; } 16  1  _objc_msgSend_stret
    /// struct ArrOne  { struct Packed9 a[1]; }               9  1  _objc_msgSend_stret
    /// struct NineChars { char c[9]; }                       9  1  _objc_msgSend
    /// ```
    ///
    /// and the LLVM IR shows the shape of the corruption exactly:
    ///
    /// ```text
    /// call void @objc_msgSend_stret(ptr … sret(%struct.Wrap) align 1 %2, …)
    /// ```
    ///
    /// — the hidden result pointer in `RDI`, which is where plain
    /// `objc_msgSend` reads `self`, i.e. the register shift
    /// [`crate::returns_indirectly`] describes. Every one of the three answers
    /// `false` under the old wording and `true` under this one. `NineChars` is
    /// the control: same size, same alignment, no packing anywhere in its tree,
    /// plain `objc_msgSend`.
    ///
    /// So the question a type's author must answer is about the type's WHOLE
    /// layout, not its outermost struct: a type is `true` here if it is packed
    /// in a way that misaligns something, **or if any field, element or nested
    /// struct of it is**.
    ///
    /// This is not an encoding: `@encode` does not record packing, and two
    /// types with the same encoding string can differ here. It lives on this
    /// trait because this trait is the ONE place a type declares its C ABI, and
    /// because it is the term [`crate::returns_indirectly`] needs and `size_of`
    /// cannot supply. On `x86_64-apple-darwin` a type with an unaligned field
    /// is classified MEMORY at ANY size — measured down to three bytes — so it
    /// must be sent through `objc_msgSend_stret` even though it is small.
    ///
    /// The default is `false` and every impl in this crate takes it: nothing in
    /// Foundation, AppKit, CoreGraphics or this crate is packed. A packed type
    /// must set it by hand, and then it works rather than corrupting registers:
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
    /// A wrapper inherits it, which is the transitive half stated as code:
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
    /// // ONE field, at offset 0, whose own alignment is 1 — so the rule this
    /// // doc used to state answers `false`. The right answer is the field's.
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
    /// Writing it as `<Field as Encode>::HAS_UNALIGNED_FIELDS` rather than a
    /// hand-typed `true` is the spelling to prefer: it cannot go stale when the
    /// field's own answer changes.
    ///
    /// # Safety
    /// Setting this to `false` for a type that misaligns a field ANYWHERE in
    /// its layout tree routes an x86_64 send to the wrong `objc_msgSend` entry
    /// point, which shifts every argument by one register. That is the same
    /// class of undefined behaviour a wrong [`Self::ENCODING`] causes, on the
    /// direct-send path rather than the reflective one.
    const HAS_UNALIGNED_FIELDS: bool = false;
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

// The four pointer-shaped runtime types, and the four different letters clang
// emits for them. ALL FOUR were `*mut c_void`/`*const c_void` aliases once, and
// two impls had to carry four meanings; each is a `#[repr(transparent)]`
// newtype now, so each can say its own letter. MEASURED on this box:
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
// SAFETY: a class object encodes as `"#"`, NOT `"@"`. Measured: `- (void)
// setDelegateClass:(Class)c` registers `v24@0:8#16`. This is the impl that used
// to be unwritable, because `ClassPtr` and `Id` were the same type.
unsafe impl Encode for crate::runtime::ClassPtr {
    const ENCODING: &'static str = "#";
}
// SAFETY: `@encode(Protocol *)` is `"@"` — a `Protocol` IS an object, and this
// one is measured rather than inferred from the name, since the neighbouring
// `Class` does not follow the same rule.
unsafe impl Encode for crate::runtime::ProtocolPtr {
    const ENCODING: &'static str = "@";
}
// SAFETY: selectors encode as `":"`. `Sel` is a `#[repr(transparent)]` newtype
// over the pointer precisely so that this impl and the two below can DISAGREE:
// they used to be the same type, and the shared impl said `":"`, so a
// `const void *context` argument would have been declared to the runtime as a
// SELECTOR.
unsafe impl Encode for crate::runtime::Sel {
    const ENCODING: &'static str = ":";
}
// A bare `void *`: an opaque caller-owned context pointer, NOT an object. THIS
// is the spelling the live sites use — the SDK's `NSKeyValueObserving.h` writes
// `context:(nullable void *)context`, `objc2-foundation`'s generated binding
// writes `context: *mut c_void`, and so does
// `vendor/winit/src/platform_impl/macos/window_delegate.rs:456`. It was mapped
// to `"@"` until this pass, i.e. registered as an OBJECT, which is worse than
// the `":"` the `Sel` newtype was introduced to prevent: `"@"` invites
// `NSInvocation` and forwarding to retain a pointer the caller owns.
// SAFETY: `void *` encodes as `"^v"` — `^` is "pointer to", `v` is `void`.
unsafe impl Encode for *mut c_void {
    const ENCODING: &'static str = "^v";
}
// The const-qualified twin. clang emits `r^v` for it, where the leading `r` is
// the `const` TYPE QUALIFIER; qualifiers are optional in a method type string
// and `NSMethodSignature` skips them, so the unqualified `"^v"` is what this
// crate registers — the same short form it uses everywhere else (`"v@:@"`, not
// `"v24@0:8@16"`).
// SAFETY: as above.
unsafe impl Encode for *const c_void {
    const ENCODING: &'static str = "^v";
}
// `char *` — what `-[NSString UTF8String]` returns.
// SAFETY: the runtime spells a C string `"*"`, distinct from the general
// `"^c"` it would otherwise be.
unsafe impl Encode for *const std::ffi::c_char {
    const ENCODING: &'static str = "*";
}

/// `@encode(BOOL *)` for the target being compiled — and it is NOT `"^"` plus
/// [`BOOL_ENCODING`] on both arms.
///
/// MEASURED, because the obvious composition is wrong on the compat slice:
///
/// ```text
/// clang -arch arm64  : @encode(BOOL*) = "^B"   (BOOL is _Bool)
/// clang -arch x86_64 : @encode(BOOL*) = "*"    (BOOL is signed char, so
///                                               BOOL* IS char*, which the
///                                               runtime spells "*")
/// ```
///
/// The arm64 figure is execution-measured; the x86_64 figure is CODEGEN-proved
/// (`clang -arch x86_64 -S`, reading the emitted string literal), because this
/// box cannot execute that slice.
#[cfg(target_arch = "aarch64")]
const BOOL_PTR_ENCODING: &str = "^B";
/// See the `aarch64` arm.
#[cfg(not(target_arch = "aarch64"))]
const BOOL_PTR_ENCODING: &str = "*";

// `BOOL *` — the `stop` out-parameter of every `enumerate…UsingBlock:` in
// Foundation, and the shape a block takes it in. Spelling it `*mut bool` in
// Rust is [`Bool`]'s rule broken through a pointer: on the x86_64 compat slice
// the framework writes a `signed char` into that byte, and materialising an
// arbitrary byte as a Rust `bool` is undefined behaviour rather than a wrong
// answer. `bool` has no `Encode` impl and neither does `*mut bool`, so the
// bound on `RcBlock::newN` refuses it and this is the only spelling that
// compiles.
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

/// An `NSRange` — a location/length pair over `NSUInteger`.
///
/// Needed because winit's `NSTextInputClient` conformance cannot be declared
/// without it: `- (NSRect)firstRectForCharacterRange:(NSRange)range
/// actualRange:(NSRangePointer)actualRange` takes one BY VALUE and one BY
/// POINTER, and `selectedRange`/`markedRange` return one. Six of the IME
/// methods `vendor/winit/src/platform_impl/macos/view.rs` declares mention it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NSRange {
    /// `NSNotFound` (`NSIntegerMax`) when there is no range.
    pub location: usize,
    /// Length in UTF-16 code units, which is what `NSTextInputClient` counts.
    pub length: usize,
}

// SAFETY: `@encode(NSRange)` is `{_NSRange=QQ}` — MEASURED with clang on this
// box. The struct TAG is `_NSRange`, not `NSRange` (`NSRange` is a typedef),
// and `Q` is `unsigned long long`, which `usize` is on every 64-bit Apple
// target. `#[repr(C)]` with two `usize` fields matches the C layout exactly and
// nothing is packed.
unsafe impl Encode for NSRange {
    const ENCODING: &'static str = "{_NSRange=QQ}";
}

// `NSRangePointer` — `NSRange *`, the out-parameter half of
// `firstRectForCharacterRange:actualRange:`. Measured: clang registers that
// method as
//
// ```text
// {CGRect={CGPoint=dd}{CGSize=dd}}40@0:8{_NSRange=QQ}16^{_NSRange=QQ}32
// ```
//
// A POINTER TO A STRUCT encodes as `^` followed by the pointee's own encoding.
// That rule is spelled out per type rather than derived, because `ENCODING` is
// a `&'static str` associated const and stable Rust has no way to concatenate
// one at compile time — `concat!` takes literals only, and building the string
// generically needs `generic_const_exprs`. A downstream crate that needs
// `^{SomeOtherStruct=…}` writes a `#[repr(transparent)]` newtype over the
// pointer and one `Encode` impl beside it, which is the same three lines.
// SAFETY: `^` is "pointer to" and `{_NSRange=QQ}` is the pointee's measured
// encoding.
unsafe impl Encode for *mut NSRange {
    const ENCODING: &'static str = "^{_NSRange=QQ}";
}
// SAFETY: as above; the `const` qualifier `r` is omitted for the reason given
// on `*const c_void`.
unsafe impl Encode for *const NSRange {
    const ENCODING: &'static str = "^{_NSRange=QQ}";
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
