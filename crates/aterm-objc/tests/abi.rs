// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The two ABI facts that differ between the arm64 slice and the x86_64 compat
//! slice `lipo`d into the same universal binary: what `BOOL` is, and which
//! `objc_msgSend` a struct return goes through.
//!
//! # What is proved here, and by what
//!
//! This box is arm64 and cannot execute the x86_64 slice ("bad CPU type"), so
//! the two halves of each claim have different evidence and the difference is
//! stated rather than blurred:
//!
//! * **arm64 — EXECUTION.** The tests below run. They do not hard-code `"B"`:
//!   they read the encoding the SYSTEM's own compiler emitted for a Foundation
//!   method and compare this crate's constant against it, so the assertion is
//!   whatever the running platform says and would fail on x86_64 if the `cfg`
//!   arms were wrong.
//! * **x86_64 — CODEGEN.** Measured with `clang -arch x86_64 -S` on this box:
//!   `@encode(BOOL)` is `"c"` and `__OBJC_BOOL_IS_BOOL` is `0`; a 32-byte
//!   struct return compiles to `callq _objc_msgSend_stret` while a 16-byte one
//!   compiles to plain `_objc_msgSend`. The Rust side's x86_64 arms are
//!   type-checked but NOT executed here.

#![cfg(target_os = "macos")]

use std::ffi::{CStr, c_char, c_void};

use aterm_objc::{
    Bool, CGPoint, CGRect, ClassPtr, Encode, Id, Sel, class, msg, returns_indirectly,
};

unsafe extern "C" {
    fn class_getInstanceMethod(cls: ClassPtr, sel: Sel) -> *const c_void;
    fn method_getTypeEncoding(m: *const c_void) -> *const c_char;
}

/// Seventeen bytes: one over the register threshold. A newtype rather than a
/// bare `[u8; 17]` because `returns_indirectly` now takes `R: Encode`, and the
/// orphan rule puts an array out of this crate's reach — which is the bound
/// doing its job, since a bare array is exactly the kind of type nobody has
/// stated an ABI for.
#[repr(C)]
struct B17([u8; 17]);
// SAFETY: `#[repr(C)]` over a 17-byte array of `signed char`; no field is
// misaligned, so the default `HAS_UNALIGNED_FIELDS = false` holds.
unsafe impl Encode for B17 {
    const ENCODING: &'static str = "{B17=[17c]}";
}

/// Twenty-four bytes: three eightbytes, well past the threshold.
#[repr(C)]
struct B24([u8; 24]);
// SAFETY: as `B17`.
unsafe impl Encode for B24 {
    const ENCODING: &'static str = "{B24=[24c]}";
}

/// Exactly sixteen bytes, naturally aligned: the last size that still comes
/// back in registers.
#[repr(C)]
struct B16([u64; 2]);
// SAFETY: `#[repr(C)]` over two `unsigned long long`, both at their natural
// alignment.
unsafe impl Encode for B16 {
    const ENCODING: &'static str = "{B16=[2Q]}";
}

/// The judge's counterexample, verbatim: `struct __attribute__((packed)) { char
/// c; long q; }`. Nine bytes, so `size_of` alone says "registers"; the `q` sits
/// at offset 1, so the System V x86-64 classification is MEMORY and clang sends
/// it through `_objc_msgSend_stret`. MEASURED — see `returns_indirectly`'s
/// table.
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

/// The judge's THIRD-pass counterexample: a WRAPPER, not a packed struct.
///
/// `HAS_UNALIGNED_FIELDS` was documented non-transitively — "whether `Self`
/// places any field at an offset that is not a multiple of that field's own
/// alignment". This type has exactly ONE field, at offset 0, whose own
/// alignment is 1, so the documented rule answers `false` for it. CODEGEN-PROVED
/// with `clang -arch x86_64 -O1 -S` (this box cannot execute that slice) that
/// the right answer is `true`: the call lands on `_objc_msgSend_stret`, and the
/// LLVM IR reads
///
/// ```text
/// call void @objc_msgSend_stret(ptr … sret(%struct.Wrap) align 1 %2, …)
/// ```
///
/// — the hidden result pointer in `RDI`, i.e. exactly the register shift
/// `returns_indirectly` describes. The rule is now stated transitively, and
/// the impl below writes the answer as the FIELD's rather than hand-typing
/// `true`, so it cannot go stale.
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

/// The same defect through an ARRAY rather than a field: `struct { Packed9 a[1]; }`.
/// Also `_objc_msgSend_stret`, also `false` under the old wording.
#[repr(C)]
struct ArrayOfOnePacked {
    a: [PackedCharLong; 1],
}
// SAFETY: as `WrapPacked`; an array of a misaligning type misaligns.
unsafe impl Encode for ArrayOfOnePacked {
    const ENCODING: &'static str = "{ArrayOfOnePacked=[1{PackedCharLong=cq}]}";
    const HAS_UNALIGNED_FIELDS: bool = <PackedCharLong as Encode>::HAS_UNALIGNED_FIELDS;
}

/// The third shape, and the one that kills any "but it is under 16 bytes"
/// intuition from the other side: padded out to exactly 16 bytes, still
/// `_objc_msgSend_stret`.
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

    // Each of the three has ONE outermost field (or element) at offset 0 whose
    // own alignment is 1 — so the non-transitive wording this test was written
    // to refute answers `false` for all three.
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

    // The claim itself, arch-independently: all three answer `true`, and the
    // non-transitive wording would have answered `false` for all three. This
    // assertion is the one that fires on THIS box; the entry-point consequence
    // below is x86_64-only and therefore codegen-proved, not run.
    // `const` blocks, so a wrong answer is a BUILD failure rather than a test
    // failure — these are associated consts, and clippy is right that asserting
    // one at run time is the weaker spelling.
    const {
        assert!(<WrapPacked as Encode>::HAS_UNALIGNED_FIELDS);
        assert!(<ArrayOfOnePacked as Encode>::HAS_UNALIGNED_FIELDS);
        assert!(<WrapPaddedTo16 as Encode>::HAS_UNALIGNED_FIELDS);
        assert!(!<NineChars as Encode>::HAS_UNALIGNED_FIELDS);
        assert!(!<B16 as Encode>::HAS_UNALIGNED_FIELDS);
    }

    // CODEGEN-PROVED, `clang -arch x86_64 -O1 -S`, reading which symbol the
    // call lands on. Every row here is `_objc_msgSend_stret`:
    //
    //   struct Wrap    { struct Packed9 inner; }               9  1  stret
    //   struct WrapPad { struct Packed9 inner; char pad[7]; }  16  1  stret
    //   struct ArrOne  { struct Packed9 a[1]; }                9  1  stret
    //   struct NineChars { char c[9]; }                        9  1  plain
    assert_eq!(returns_indirectly::<WrapPacked>(), x86);
    assert_eq!(returns_indirectly::<ArrayOfOnePacked>(), x86);
    assert_eq!(returns_indirectly::<WrapPaddedTo16>(), x86);

    // `NineChars` is the control and the reason this is not a size rule in
    // disguise: same 9 bytes, same alignment 1, no packing anywhere in its
    // tree, plain `objc_msgSend`.
    assert!(!returns_indirectly::<NineChars>());

    // The pair `WrapPaddedTo16` / `B16` is the sharpest form of it: both are
    // exactly 16 bytes, and on x86_64 they take DIFFERENT entry points. (On
    // arm64 there is one entry point and both are `false`, which is the whole
    // reason this crate's x86_64 half is codegen-proved rather than run.)
    assert_eq!(size_of::<WrapPaddedTo16>(), size_of::<B16>());
    assert!(!returns_indirectly::<B16>(), "16 aligned bytes: registers");
    assert_eq!(
        returns_indirectly::<WrapPaddedTo16>() != returns_indirectly::<B16>(),
        x86,
        "on x86_64 two 16-byte returns must disagree, or the rule has collapsed \
         back into a size test"
    );

    // And the prototypes resolve to different SYMBOLS, which is the whole
    // consequence. Only the arm64 arm executes on this box.
    // SAFETY: neither pointer is called — the test only compares addresses.
    let (plain, wrapped) = unsafe {
        let plain: unsafe extern "C" fn(Id, Sel) -> B16 = msg();
        let wrapped: unsafe extern "C" fn(Id, Sel) -> WrapPaddedTo16 = msg();
        (plain as usize, wrapped as usize)
    };
    if x86 {
        assert_ne!(plain, wrapped);
    } else {
        assert_eq!(plain, wrapped, "arm64 has one entry point");
    }
}

/// The type encoding the SYSTEM compiler emitted for `cls`'s `name`, or `None`
/// if that class does not implement it.
fn platform_encoding(cls: &'static CStr, name: &'static CStr) -> Option<String> {
    let cls = class(cls);
    assert!(!cls.is_null(), "{cls:?} is not linked into this binary");
    // SAFETY: `cls` is a live, immortal class object and the two calls are
    // side-effect-free runtime queries; the string they return is owned by the
    // runtime and outlives the read.
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
    // `-[NSObject isEqual:]` and `-[NSObject respondsToSelector:]` both return
    // `BOOL`, and their encodings were written by the compiler that built
    // Foundation for THIS architecture. Compiler-emitted encodings carry byte
    // offsets ("B24@0:8@16"), so only the leading return-type letter is
    // compared — which is the whole claim.
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

    // The value side of the same fact: one byte on both arches, and the
    // conversion is `!= 0` rather than a transmute, so no bit pattern is
    // invalid. On x86_64 `BOOL` is `signed char` and a sender may legally pass
    // any non-zero byte for YES; a Rust `bool` there would be UB.
    assert_eq!(size_of::<Bool>(), 1);
    assert!(Bool::YES.as_bool());
    assert!(!Bool::NO.as_bool());
    assert_eq!(Bool::default(), Bool::NO);
    assert_eq!(Bool::from(true), Bool::YES);
    assert!(bool::from(Bool::YES));
    assert_eq!(
        Bool::ENCODING,
        if cfg!(target_arch = "aarch64") {
            "B"
        } else {
            "c"
        },
        "the cfg arms and the measurement disagree"
    );
}

#[test]
fn the_struct_encodings_match_what_foundation_itself_emits() {
    // Same technique against `CGRect`: `-[NSValue rectValue]` is compiled by
    // the system compiler, so its encoding is the platform's own spelling of
    // the layout this crate hard-codes.
    let Some(enc) = platform_encoding(c"NSValue", c"rectValue") else {
        return;
    };
    assert!(
        enc.starts_with(CGRect::ENCODING),
        "Foundation spells NSRect {enc:?}; this crate says {:?}",
        CGRect::ENCODING
    );
}

#[test]
fn the_indirect_return_rule_matches_the_c_abi() {
    // MEASURED with clang, `-arch x86_64 -S -O1`, one selector per return type:
    //
    //   struct of 2 long   (16 bytes) -> _objc_msgSend
    //   struct of 2 double (16 bytes) -> _objc_msgSend
    //   char[17]           (17 bytes) -> _objc_msgSend_stret
    //   struct of 3 double (24 bytes) -> _objc_msgSend_stret
    //   NSRect             (32 bytes) -> _objc_msgSend_stret
    //   float / double                -> _objc_msgSend
    //   long double                   -> _objc_msgSend_fpret  (not expressible
    //                                    in Rust, so not bound)
    //
    // The threshold is therefore exactly "larger than 16 bytes", and on arm64
    // there is no threshold at all because the indirect-result pointer has its
    // own register.
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

    // The size-only rule got this one WRONG, silently, and it was the crate's
    // only fail-silent gap. Nine bytes is under the 16-byte threshold, so the
    // old `size_of`-only test said "registers"; clang sends it through
    // `_objc_msgSend_stret`, which would have shifted every argument by one
    // register on the slice this repo ships.
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
    // And the discriminating pair: `size_of` and `align_of` cannot tell these
    // apart, so no `const` predicate over them could have closed this — the
    // answer has to come from the type's author. Both are 9/1; only one is
    // MEMORY.
    assert_eq!(
        (size_of::<PackedCharLong>(), align_of::<PackedCharLong>()),
        (size_of::<NineChars>(), align_of::<NineChars>()),
    );
    assert!(
        !returns_indirectly::<NineChars>(),
        "nine bytes of char come back in registers — measured with clang"
    );
}

/// Nine bytes, alignment one, NOT packed: `struct { char c[9]; }`, which clang
/// sends through plain `_objc_msgSend`. Same size and same alignment as
/// [`PackedCharLong`], opposite ABI.
#[repr(C)]
struct NineChars([u8; 9]);
// SAFETY: `#[repr(C)]` over a 9-byte char array; nothing is misaligned.
unsafe impl Encode for NineChars {
    const ENCODING: &'static str = "{NineChars=[9c]}";
}

#[test]
fn the_entry_point_is_chosen_by_the_return_type() {
    // `msg` hands back a function pointer; which SYMBOL it points at is the
    // whole of D4. On x86_64 a >16-byte return must land on a different symbol
    // from an object return; on arm64 there is only one symbol, and asserting
    // that is what proves the crate is not smuggling a `_stret` reference into
    // a binary whose `libobjc` does not export one (that would be a load-time
    // failure in the shipped app, not a compile error here).
    //
    // Only the arm64 arm EXECUTES on this box; the x86_64 arm is type-checked.
    // SAFETY: neither pointer is called — the test only compares addresses.
    let (plain, big, point) = unsafe {
        let plain: unsafe extern "C" fn(Id, Sel) -> Id = msg();
        let big: unsafe extern "C" fn(Id, Sel) -> CGRect = msg();
        let point: unsafe extern "C" fn(Id, Sel) -> CGPoint = msg();
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
