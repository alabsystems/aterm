// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ABI tests for `BOOL` and struct-return `objc_msgSend` across arm64 and x86_64.

#![cfg(target_os = "macos")]
#![expect(
    clippy::disallowed_methods,
    reason = "aterm-objc's own runtime tests drive the bare `msg` cast; everything else uses checked_msg"
)]

use std::ffi::{CStr, c_char, c_void};

use aterm_objc::{
    Bool, CGPoint, CGRect, ClassPtr, Encode, Id, Sel, class, msg, returns_indirectly,
};

unsafe extern "C" {
    fn class_getInstanceMethod(cls: ClassPtr, sel: Sel) -> *const c_void;
    fn method_getTypeEncoding(m: *const c_void) -> *const c_char;
}

/// Seventeen bytes.
#[repr(C)]
struct B17([u8; 17]);
// SAFETY: `#[repr(C)]` over a 17-byte char array; nothing is misaligned.
unsafe impl Encode for B17 {
    const ENCODING: &'static str = "{B17=[17c]}";
}

/// Twenty-four bytes.
#[repr(C)]
struct B24([u8; 24]);
// SAFETY: as `B17`.
unsafe impl Encode for B24 {
    const ENCODING: &'static str = "{B24=[24c]}";
}

/// Exactly sixteen bytes, naturally aligned.
#[repr(C)]
struct B16([u64; 2]);
// SAFETY: `#[repr(C)]` over two `unsigned long long`, both at their natural alignment.
unsafe impl Encode for B16 {
    const ENCODING: &'static str = "{B16=[2Q]}";
}

/// `struct __attribute__((packed)) { char c; long q; }`.
#[repr(C, packed)]
struct PackedCharLong {
    c: i8,
    q: i64,
}
// SAFETY: the layout is `{PackedCharLong=cq}`, and the `q` at offset 1 is below
// its own 8-byte alignment, which is the whole reason this impl exists.
unsafe impl Encode for PackedCharLong {
    const ENCODING: &'static str = "{PackedCharLong=cq}";
    const HAS_UNALIGNED_FIELDS: bool = true;
}

/// A wrapper around a packed struct.
#[repr(C)]
struct WrapPacked {
    inner: PackedCharLong,
}
// SAFETY: `#[repr(C)]` around one `{PackedCharLong=cq}`; the misalignment is
// the field's and `WrapPacked` inherits it, which is what the transitive rule
// says and what clang's classification does.
unsafe impl Encode for WrapPacked {
    const ENCODING: &'static str = "{WrapPacked={PackedCharLong=cq}}";
    const HAS_UNALIGNED_FIELDS: bool = <PackedCharLong as Encode>::HAS_UNALIGNED_FIELDS;
}

/// The same through an array, `struct { Packed9 a[1]; }`.
#[repr(C)]
struct ArrayOfOnePacked {
    a: [PackedCharLong; 1],
}
// SAFETY: as `WrapPacked`; an array of a misaligning type misaligns.
unsafe impl Encode for ArrayOfOnePacked {
    const ENCODING: &'static str = "{ArrayOfOnePacked=[1{PackedCharLong=cq}]}";
    const HAS_UNALIGNED_FIELDS: bool = <PackedCharLong as Encode>::HAS_UNALIGNED_FIELDS;
}

/// Padded to exactly 16 bytes, and still `_objc_msgSend_stret`.
#[repr(C)]
struct WrapPaddedTo16 {
    inner: PackedCharLong,
    pad: [u8; 7],
}
// SAFETY: as `WrapPacked`.
unsafe impl Encode for WrapPaddedTo16 {
    const ENCODING: &'static str = "{WrapPaddedTo16={PackedCharLong=cq}[7c]}";
    const HAS_UNALIGNED_FIELDS: bool = <PackedCharLong as Encode>::HAS_UNALIGNED_FIELDS;
}

#[test]
fn the_unaligned_rule_is_transitive_and_a_wrapper_proves_it() {
    let x86 = cfg!(target_arch = "x86_64");

    assert_eq!((size_of::<WrapPacked>(), align_of::<WrapPacked>()), (9, 1));
    assert_eq!(
        (
            size_of::<ArrayOfOnePacked>(),
            align_of::<ArrayOfOnePacked>()
        ),
        (9, 1)
    );
    assert_eq!(
        (size_of::<WrapPaddedTo16>(), align_of::<WrapPaddedTo16>()),
        (16, 1)
    );
    assert!(
        size_of::<WrapPaddedTo16>() <= 16,
        "under the size threshold"
    );

    const {
        assert!(<WrapPacked as Encode>::HAS_UNALIGNED_FIELDS);
        assert!(<ArrayOfOnePacked as Encode>::HAS_UNALIGNED_FIELDS);
        assert!(<WrapPaddedTo16 as Encode>::HAS_UNALIGNED_FIELDS);
        assert!(!<NineChars as Encode>::HAS_UNALIGNED_FIELDS);
        assert!(!<B16 as Encode>::HAS_UNALIGNED_FIELDS);
    }

    assert_eq!(returns_indirectly::<WrapPacked>(), x86);
    assert_eq!(returns_indirectly::<ArrayOfOnePacked>(), x86);
    assert_eq!(returns_indirectly::<WrapPaddedTo16>(), x86);

    assert!(!returns_indirectly::<NineChars>());

    assert_eq!(size_of::<WrapPaddedTo16>(), size_of::<B16>());
    assert!(!returns_indirectly::<B16>(), "16 aligned bytes: registers");
    assert_eq!(
        returns_indirectly::<WrapPaddedTo16>() != returns_indirectly::<B16>(),
        x86,
        "on x86_64 two 16-byte returns must disagree, or the rule has collapsed \
         back into a size test"
    );

    // SAFETY: neither pointer is called — the test only compares addresses.
    let (plain, wrapped) = unsafe {
        let plain: unsafe extern "C-unwind" fn(Id, Sel) -> B16 = msg();
        let wrapped: unsafe extern "C-unwind" fn(Id, Sel) -> WrapPaddedTo16 = msg();
        (plain as usize, wrapped as usize)
    };
    if x86 {
        assert_ne!(plain, wrapped);
    } else {
        assert_eq!(plain, wrapped, "arm64 has one entry point");
    }
}

/// The type encoding the system compiler emitted for `cls`'s `name`.
fn platform_encoding(cls: &'static CStr, name: &'static CStr) -> Option<String> {
    let cls = class(cls);
    assert!(!cls.is_null(), "{cls:?} is not linked into this binary");
    // SAFETY: `cls` is a live class object; the runtime owns the returned encoding string, which
    // outlives the read.
    unsafe {
        let m = class_getInstanceMethod(cls, aterm_objc::sel_uncached(name));
        if m.is_null() {
            return None;
        }
        let e = method_getTypeEncoding(m);
        if e.is_null() {
            return None;
        }
        Some(CStr::from_ptr(e).to_string_lossy().into_owned())
    }
}

#[test]
fn bool_encoding_matches_what_foundation_itself_emits() {
    let mut checked = 0;
    for sel in [c"isEqual:", c"respondsToSelector:", c"isProxy"] {
        let Some(enc) = platform_encoding(c"NSObject", sel) else {
            continue;
        };
        let leading = &enc[..1];
        assert_eq!(
            leading,
            Bool::ENCODING,
            "Foundation encodes -[NSObject {sel:?}]'s BOOL return as {leading:?} \
             but this crate says {:?} (full encoding {enc:?})",
            Bool::ENCODING
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "no BOOL-returning NSObject method was readable"
    );
}

#[test]
fn the_struct_encodings_match_what_foundation_itself_emits() {
    let Some(enc) = platform_encoding(c"NSValue", c"rectValue") else {
        aterm_spec::host::could_not_run("Foundation here has no -[NSValue rectValue] to read");
    };
    assert!(
        enc.starts_with(CGRect::ENCODING),
        "Foundation spells NSRect {enc:?}; this crate says {:?}",
        CGRect::ENCODING
    );
}

#[test]
fn the_indirect_return_rule_matches_the_c_abi() {
    let x86 = cfg!(target_arch = "x86_64");
    assert_eq!(returns_indirectly::<CGRect>(), x86, "CGRect is 32 bytes");
    assert_eq!(returns_indirectly::<B17>(), x86, "17 bytes");
    assert_eq!(returns_indirectly::<B24>(), x86, "24 bytes");
    assert!(!returns_indirectly::<CGPoint>(), "CGPoint is 16 bytes");
    assert!(!returns_indirectly::<B16>(), "16 bytes");
    assert!(!returns_indirectly::<f64>());
    assert!(!returns_indirectly::<Id>());
    assert!(!returns_indirectly::<Bool>());
    assert!(!returns_indirectly::<()>());

    assert_eq!(size_of::<PackedCharLong>(), 9);
    assert_eq!(align_of::<PackedCharLong>(), 1);
    assert!(
        size_of::<PackedCharLong>() <= 16,
        "under the size threshold"
    );
    assert_eq!(
        returns_indirectly::<PackedCharLong>(),
        x86,
        "a 9-byte packed struct is MEMORY-classed on x86_64 despite its size"
    );
    assert_eq!(
        (size_of::<PackedCharLong>(), align_of::<PackedCharLong>()),
        (size_of::<NineChars>(), align_of::<NineChars>()),
    );
    assert!(
        !returns_indirectly::<NineChars>(),
        "nine bytes of char come back in registers — measured with clang"
    );
}

/// Nine bytes, alignment one, not packed ([`PackedCharLong`]).
#[repr(C)]
struct NineChars([u8; 9]);
// SAFETY: `#[repr(C)]` over a 9-byte char array; nothing is misaligned.
unsafe impl Encode for NineChars {
    const ENCODING: &'static str = "{NineChars=[9c]}";
}

#[test]
fn the_entry_point_is_chosen_by_the_return_type() {
    // SAFETY: neither pointer is called — the test only compares addresses.
    let (plain, big, point) = unsafe {
        let plain: unsafe extern "C-unwind" fn(Id, Sel) -> Id = msg();
        let big: unsafe extern "C-unwind" fn(Id, Sel) -> CGRect = msg();
        let point: unsafe extern "C-unwind" fn(Id, Sel) -> CGPoint = msg();
        (plain as usize, big as usize, point as usize)
    };
    if cfg!(target_arch = "x86_64") {
        assert_ne!(
            plain, big,
            "a 32-byte return must go through objc_msgSend_stret on x86_64"
        );
        assert_eq!(point, plain, "a 16-byte return stays on plain objc_msgSend");
    } else {
        assert_eq!(
            plain, big,
            "arm64 libobjc exports no _stret variant, so every return type must \
             resolve to the one entry point"
        );
        assert_eq!(point, plain);
    }
}
