// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Objective-C type encodings (`@encode`) and C ABI layout traits ([`Encode`]).

use std::ffi::c_void;
use std::fmt;

/// Objective-C type encoding of a value that can cross a method boundary.
///
/// # Safety
/// `ENCODING` must match the C ABI encoding of `Self`.
pub unsafe trait Encode {
    /// Objective-C type-encoding string (e.g. `"@"` for an object or `"d"` for `CGFloat`).
    const ENCODING: &'static str;

    /// Whether `Self` contains any field at an offset not aligned to that field's alignment ([`crate::returns_indirectly`]).
    ///
    /// # Safety
    /// Setting `false` when a field is misaligned routes `x86_64` sends to the wrong entry point.
    const HAS_UNALIGNED_FIELDS: bool = false;
}

macro_rules! prim {
    ($($t:ty => $e:literal),* $(,)?) => {$(
        // SAFETY: The letters are the runtime's documented encodings and match each Rust type's C ABI on 64-bit Apple targets.
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

/// Objective-C `BOOL` at the target's native width, values, and encoding.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Bool(BoolRepr);

#[cfg(target_arch = "aarch64")]
impl Bool {
    /// Objective-C `NO`.
    pub const NO: Self = Self(false);
    /// Objective-C `YES`.
    pub const YES: Self = Self(true);

    /// Converts a Rust `bool` into a [`Bool`].
    #[inline]
    #[must_use]
    pub const fn new(value: bool) -> Self {
        Self(value)
    }

    /// Converts this [`Bool`] into a Rust `bool`.
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

    /// Converts a Rust `bool` into a [`Bool`].
    #[inline]
    #[must_use]
    pub const fn new(value: bool) -> Self {
        Self(value as BoolRepr)
    }

    /// Converts this [`Bool`] into a Rust `bool` (`!= 0`).
    #[inline]
    #[must_use]
    pub const fn as_bool(self) -> bool {
        self.0 != 0
    }
}

impl From<bool> for Bool {
    #[inline]
    fn from(value: bool) -> Self {
        Self::new(value)
    }
}

impl From<Bool> for bool {
    #[inline]
    fn from(value: Bool) -> Self {
        value.as_bool()
    }
}

impl fmt::Debug for Bool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.as_bool() { "YES" } else { "NO" })
    }
}

// SAFETY: `Bool` is `#[repr(transparent)]` over the target's C `BOOL` and `BOOL_ENCODING` is its `@encode`.
unsafe impl Encode for Bool {
    const ENCODING: &'static str = BOOL_ENCODING;
}

// SAFETY: Object pointers encode as `"@"`, and `Id` is `#[repr(transparent)]` over the runtime pointer.
unsafe impl Encode for crate::runtime::Id {
    const ENCODING: &'static str = "@";
}
// SAFETY: Class objects encode as `"#"`.
unsafe impl Encode for crate::runtime::ClassPtr {
    const ENCODING: &'static str = "#";
}
// SAFETY: `@encode(Protocol *)` is `"@"`.
unsafe impl Encode for crate::runtime::ProtocolPtr {
    const ENCODING: &'static str = "@";
}
// SAFETY: Selectors encode as `":"`.
unsafe impl Encode for crate::runtime::Sel {
    const ENCODING: &'static str = ":";
}
// SAFETY: `void *` encodes as `"^v"`.
unsafe impl Encode for *mut c_void {
    const ENCODING: &'static str = "^v";
}
// SAFETY: `const void *` encodes as `"^v"` (the `r` qualifier is optional).
unsafe impl Encode for *const c_void {
    const ENCODING: &'static str = "^v";
}
// SAFETY: C strings (`char *`) encode as `"*"`.
unsafe impl Encode for *const std::ffi::c_char {
    const ENCODING: &'static str = "*";
}
// SAFETY: `unsigned char *` encodes as `"*"` under clang on both Apple architectures.
unsafe impl Encode for *mut std::ffi::c_uchar {
    const ENCODING: &'static str = "*";
}

// SAFETY: A pointer to object pointers (`id *`) encodes as `"^@"`.
unsafe impl Encode for *const crate::runtime::Id {
    const ENCODING: &'static str = "^@";
}
// SAFETY: A pointer to C strings (`unsigned char **`) encodes as `"^*"`.
unsafe impl Encode for *mut *mut std::ffi::c_uchar {
    const ENCODING: &'static str = "^*";
}

// SAFETY: `const uint8_t *` byte buffers encode as `"r*"`.
unsafe impl Encode for *const u8 {
    const ENCODING: &'static str = "r*";
}

// SAFETY: `const uint32_t *` word buffers encode as `"r^I"`.
unsafe impl Encode for *const u32 {
    const ENCODING: &'static str = "r^I";
}

// SAFETY: `unichar *` (`*mut u16`) encodes as `"^S"`.
unsafe impl Encode for *mut u16 {
    const ENCODING: &'static str = "^S";
}

// SAFETY: `const unichar *` (`*const u16`) encodes as `"^S"`.
unsafe impl Encode for *const u16 {
    const ENCODING: &'static str = "^S";
}

// SAFETY: `id *` out-parameters (`NSError **`) encode as `"^@"`.
unsafe impl Encode for *mut crate::runtime::Id {
    const ENCODING: &'static str = "^@";
}

/// `@encode(BOOL *)` for the target being compiled.
#[cfg(target_arch = "aarch64")]
const BOOL_PTR_ENCODING: &str = "^B";
/// See the `aarch64` arm.
#[cfg(not(target_arch = "aarch64"))]
const BOOL_PTR_ENCODING: &str = "^c";

// SAFETY: `*mut Bool` matches `@encode(BOOL *)` (`BOOL_PTR_ENCODING`) on both Apple architectures.
unsafe impl Encode for *mut Bool {
    const ENCODING: &'static str = BOOL_PTR_ENCODING;
}

/// CoreGraphics `CGPoint` / AppKit `NSPoint`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

impl CGPoint {
    /// Constructs a `CGPoint` at `(x, y)`.
    #[inline]
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// CoreGraphics `CGSize` / AppKit `NSSize`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

impl CGSize {
    /// Constructs a `CGSize` of `(width, height)`.
    #[inline]
    #[must_use]
    pub const fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }
}

/// CoreGraphics `CGRect` / AppKit `NSRect`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

impl CGRect {
    /// Constructs a `CGRect` from `origin` and `size`.
    #[inline]
    #[must_use]
    pub const fn new(origin: CGPoint, size: CGSize) -> Self {
        Self { origin, size }
    }
}

// SAFETY: `#[repr(C)]` with two `f64` fields matches Foundation's `{CGPoint=dd}` layout.
unsafe impl Encode for CGPoint {
    const ENCODING: &'static str = "{CGPoint=dd}";
}
// SAFETY: `#[repr(C)]` with two `f64` fields matches Foundation's `{CGSize=dd}` layout.
unsafe impl Encode for CGSize {
    const ENCODING: &'static str = "{CGSize=dd}";
}
// SAFETY: `#[repr(C)]` over `CGPoint` and `CGSize` matches Foundation's `{CGRect={CGPoint=dd}{CGSize=dd}}` layout.
unsafe impl Encode for CGRect {
    const ENCODING: &'static str = "{CGRect={CGPoint=dd}{CGSize=dd}}";
}

/// Foundation `NSRange` (`location` and `length` in UTF-16 code units).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NSRange {
    /// Start index, or `NSNotFound` (`NSIntegerMax`) when absent.
    pub location: usize,
    /// Length in UTF-16 code units.
    pub length: usize,
}

impl NSRange {
    /// Constructs an `NSRange` with `(location, length)`.
    #[inline]
    #[must_use]
    pub const fn new(location: usize, length: usize) -> Self {
        Self { location, length }
    }

    /// Whether `length == 0`.
    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.length == 0
    }

    /// Returns `location.saturating_add(length)`.
    #[inline]
    #[must_use]
    pub const fn end_saturating(self) -> usize {
        self.location.saturating_add(self.length)
    }

    /// Whether `idx` lies within `location..location + length`.
    #[inline]
    #[must_use]
    pub const fn contains_index(self, idx: usize) -> bool {
        if idx < self.location {
            return false;
        }
        (idx - self.location) < self.length
    }

    /// Encodes this `NSRange` into 16 little-endian bytes.
    #[inline]
    #[must_use]
    pub const fn encode_le(self) -> [u8; 16] {
        let loc = self.location as u64;
        let len = self.length as u64;
        [
            loc as u8,
            (loc >> 8) as u8,
            (loc >> 16) as u8,
            (loc >> 24) as u8,
            (loc >> 32) as u8,
            (loc >> 40) as u8,
            (loc >> 48) as u8,
            (loc >> 56) as u8,
            len as u8,
            (len >> 8) as u8,
            (len >> 16) as u8,
            (len >> 24) as u8,
            (len >> 32) as u8,
            (len >> 40) as u8,
            (len >> 48) as u8,
            (len >> 56) as u8,
        ]
    }

    /// Decodes 16 little-endian bytes into an `NSRange`.
    #[inline]
    #[must_use]
    pub const fn decode_le(bytes: [u8; 16]) -> Self {
        let loc = (bytes[0] as u64)
            | ((bytes[1] as u64) << 8)
            | ((bytes[2] as u64) << 16)
            | ((bytes[3] as u64) << 24)
            | ((bytes[4] as u64) << 32)
            | ((bytes[5] as u64) << 40)
            | ((bytes[6] as u64) << 48)
            | ((bytes[7] as u64) << 56);
        let len = (bytes[8] as u64)
            | ((bytes[9] as u64) << 8)
            | ((bytes[10] as u64) << 16)
            | ((bytes[11] as u64) << 24)
            | ((bytes[12] as u64) << 32)
            | ((bytes[13] as u64) << 40)
            | ((bytes[14] as u64) << 48)
            | ((bytes[15] as u64) << 56);
        Self {
            location: loc as usize,
            length: len as usize,
        }
    }
}

// SAFETY: `#[repr(C)]` with two `usize` fields matches `@encode(NSRange)` (`{_NSRange=QQ}`) on 64-bit Apple targets.
unsafe impl Encode for NSRange {
    const ENCODING: &'static str = "{_NSRange=QQ}";
}

// SAFETY: `*mut NSRange` (`NSRangePointer`) encodes as `"^{_NSRange=QQ}"`.
unsafe impl Encode for *mut NSRange {
    const ENCODING: &'static str = "^{_NSRange=QQ}";
}
// SAFETY: `*const NSRange` encodes as `"^{_NSRange=QQ}"`.
unsafe impl Encode for *const NSRange {
    const ENCODING: &'static str = "^{_NSRange=QQ}";
}

/// Strips top-level argument frame offsets from a compiler-emitted Objective-C method type encoding.
#[must_use]
pub fn strip_method_offsets(types: &str) -> String {
    let mut out = String::with_capacity(types.len());
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

/// Builds an offset-free Objective-C method type-encoding string from return and argument types.
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
