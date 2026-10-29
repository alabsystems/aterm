// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `aterm-bits` — minimal `Pod`/`Zeroable` traits and byte-casting helpers.

#![warn(missing_docs)]
#![warn(clippy::undocumented_unsafe_blocks)]
#![warn(rust_2018_idioms)]

use core::mem::{align_of, size_of, size_of_val};
use core::slice;

/// Types whose all-zero bit pattern is a valid value.
///
/// # Safety
/// Implementors must guarantee that `[0u8; size_of::<Self>()]` interpreted as `Self` is a sound, fully-initialized value.
pub unsafe trait Zeroable: Sized {
    /// Returns the all-zero bit pattern for this type.
    #[inline]
    #[must_use]
    fn zeroed() -> Self {
        // SAFETY: by the `Zeroable` trait contract, the all-zero bit pattern is a valid value of `Self`.
        unsafe { core::mem::zeroed() }
    }
}

/// Plain-old-data.
///
/// # Safety
/// Implementors must satisfy *every* bullet:
///
/// 1. The type is `Copy`.
/// 2. The type is `'static` — no lifetimes.
/// 3. The type contains no references, raw pointers, function pointers,
///    `NonZero*`, or any niche-optimized discriminants.
/// 4. The type has a well-defined layout (`#[repr(C)]`, `#[repr(transparent)]`,
///    or a primitive).
/// 5. There are **no padding bytes** with indeterminate contents: the declared
///    fields cover every byte (explicit `_padding: [u8; N]` fields).
/// 6. Every field of the type is itself `Pod`.
pub unsafe trait Pod: Zeroable + Copy + 'static {}

macro_rules! impl_pod_zeroable_primitive {
    ($($t:ty),* $(,)?) => {
        $(
            // SAFETY: `$t` is a built-in primitive integer/float. All bit
            // patterns of the appropriate size are valid values of the type,
            // and the all-zero bit pattern corresponds to the value `0` /
            // `0.0` which is always valid.
            unsafe impl Zeroable for $t {}
            // SAFETY: see above — `$t` is `Copy + 'static`, has a canonical layout, and every bit pattern is a valid value.
            unsafe impl Pod for $t {}
        )*
    };
}

impl_pod_zeroable_primitive!(u8, u16, u32, u64, u128, usize);
impl_pod_zeroable_primitive!(i8, i16, i32, i64, i128, isize);
impl_pod_zeroable_primitive!(f32, f64);

// SAFETY: `()` is a ZST; there are no bytes, so trivially every bit pattern (i.e. the empty one) is
// valid and all-zero is sound.
unsafe impl Zeroable for () {}
// SAFETY: see above.
unsafe impl Pod for () {}

// SAFETY: `[T; N]` has layout `T` repeated `N` times with no extra padding
// (stable guarantee). If every `T` byte pattern is valid, so is every
// `[T; N]` byte pattern. Same argument for `Zeroable`.
unsafe impl<T: Zeroable, const N: usize> Zeroable for [T; N] {}
// SAFETY: see above; additionally `[T; N]: Copy` when `T: Copy`, and `[T; N]` is `'static` when `T: 'static`.
unsafe impl<T: Pod, const N: usize> Pod for [T; N] {}

/// Reason a byte cast failed at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PodCastError {
    /// The target alignment is stricter than the source alignment.
    TargetAlignmentGreaterAndInputNotAligned,
    /// The source byte length is not an integer multiple of the target size.
    OutputSliceWouldHaveSlop,
    /// One of the element types is zero-sized and the other is not.
    SizeMismatch,
}

impl core::fmt::Display for PodCastError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let msg = match self {
            Self::TargetAlignmentGreaterAndInputNotAligned => {
                "target alignment is greater than the input slice's alignment"
            }
            Self::OutputSliceWouldHaveSlop => {
                "input byte length is not an integer multiple of the output element size"
            }
            Self::SizeMismatch => "input slice is smaller than one output value",
        };
        f.write_str(msg)
    }
}

impl core::error::Error for PodCastError {}

/// Panics with `"<op> failed: <message>"` matching the [`PodCastError`] `Display` text.
macro_rules! panic_cast_failed {
    ($op:literal, $err:expr) => {
        match $err {
            PodCastError::TargetAlignmentGreaterAndInputNotAligned => panic!(concat!(
                $op,
                " failed: target alignment is greater than the input slice's alignment"
            )),
            PodCastError::OutputSliceWouldHaveSlop => panic!(concat!(
                $op,
                " failed: input byte length is not an integer multiple of the output element size"
            )),
            PodCastError::SizeMismatch => panic!(concat!(
                $op,
                " failed: input slice is smaller than one output value"
            )),
        }
    };
}

/// Cast `&[A]` to `&[B]` where both are `Pod`.
///
/// # Panics
/// Panics whenever [`try_cast_slice`] returns an error, in check order:
/// * If `size_of::<B>() == 0` and `size_of::<A>() != 0` (or vice versa).
/// * If `src.as_ptr()` is not aligned to `align_of::<B>()`.
/// * If `size_of_val(src)` is not an integer multiple of `size_of::<B>()`.
#[inline]
#[must_use]
pub fn cast_slice<A: Pod, B: Pod>(src: &[A]) -> &[B] {
    match try_cast_slice(src) {
        Ok(out) => out,
        Err(err) => panic_cast_failed!("cast_slice", err),
    }
}

/// Fallible [`cast_slice`].
#[inline]
pub fn try_cast_slice<A: Pod, B: Pod>(src: &[A]) -> Result<&[B], PodCastError> {
    let a_size = size_of::<A>();
    let b_size = size_of::<B>();
    let byte_len = size_of_val(src);

    if a_size == 0 || b_size == 0 {
        if a_size == b_size {
            return Ok(&[]);
        }
        return Err(PodCastError::SizeMismatch);
    }

    let ptr_a = src.as_ptr();

    if !(ptr_a as usize).is_multiple_of(align_of::<B>()) {
        return Err(PodCastError::TargetAlignmentGreaterAndInputNotAligned);
    }

    if !byte_len.is_multiple_of(b_size) {
        return Err(PodCastError::OutputSliceWouldHaveSlop);
    }

    let out_len = byte_len / b_size;
    let ptr_b = ptr_a.cast::<B>();
    if (ptr_b as usize) == 0 {
        // Non-null proof for the verifier across the pointer cast.
        return Ok(&[]);
    }
    // SAFETY: the checks above (all on this path) verified:
    //   * `ptr_b` is non-null and aligned to `align_of::<B>()`.
    //   * `out_len * size_of::<B>() <= byte_len == size_of_val(src)` bytes of
    //     initialized memory are reachable from `ptr_b` (it is `src`'s own
    //     data pointer).
    //   * Both `A` and `B` are `Pod`, so every bit pattern of
    //     `size_of::<B>() * out_len` bytes is a valid `[B; out_len]`.
    //   * The returned lifetime is tied to `&[A]`, so `B` cannot outlive it.
    // Therefore `from_raw_parts` produces a sound `&[B]`.
    Ok(unsafe { slice::from_raw_parts(ptr_b, out_len) })
}

/// View a single `Pod` value as its raw bytes.
#[inline]
#[must_use]
pub const fn bytes_of<T: Pod>(value: &T) -> &[u8] {
    let ptr = core::ptr::from_ref(value).cast::<u8>();
    if ptr.is_null() {
        // Non-null proof for the verifier across the pointer cast.
        return &[];
    }
    // SAFETY: `ptr` is non-null (guarded above), trivially aligned for `u8`,
    // and points at `value`, so exactly `size_of::<T>()` bytes are reachable;
    // `T: Pod` guarantees those bytes are fully initialized (no padding)
    // with a well-defined layout. The returned slice borrows from `value`,
    // so its lifetime is bounded correctly.
    unsafe { slice::from_raw_parts(ptr, size_of::<T>()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cast_slice_u32_to_u8_native_endian() {
        let src: [u32; 2] = [0xDEAD_BEEF, 0x1234_5678];
        let bytes: &[u8] = cast_slice(&src);
        assert_eq!(bytes.len(), 8);
        let mut recovered = [0u32; 2];
        recovered[0] = u32::from_ne_bytes(bytes[0..4].try_into().unwrap());
        recovered[1] = u32::from_ne_bytes(bytes[4..8].try_into().unwrap());
        assert_eq!(recovered, src);
    }

    #[test]
    fn cast_slice_roundtrip_f32_u8() {
        let src: [f32; 3] = [1.0, -2.5, core::f32::consts::PI];
        let bytes: &[u8] = cast_slice(&src);
        assert_eq!(bytes.len(), 12);
        let back: &[f32] = try_cast_slice(bytes).expect("aligned buffer");
        assert_eq!(back, &src);
    }

    #[test]
    fn try_cast_slice_slop_returns_err() {
        let buf: [u32; 2] = [0, 0];
        let bytes: &[u8] = cast_slice(&buf);
        let truncated = &bytes[..6];
        assert_eq!((truncated.as_ptr() as usize) % align_of::<u32>(), 0);
        let result: Result<&[u32], _> = try_cast_slice(truncated);
        assert_eq!(result, Err(PodCastError::OutputSliceWouldHaveSlop));
    }

    #[test]
    fn bytes_of_single_value() {
        let v: u32 = 0x0102_0304;
        let bytes = bytes_of(&v);
        assert_eq!(bytes.len(), 4);
        assert_eq!(u32::from_ne_bytes(bytes.try_into().unwrap()), v);
    }

    fn panic_message(f: impl FnOnce() + std::panic::UnwindSafe) -> String {
        let payload = std::panic::catch_unwind(f).expect_err("closure must panic");
        if let Some(s) = payload.downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            panic!("non-string panic payload")
        }
    }

    /// The static literals in `panic_cast_failed!` must stay byte-identical
    /// to `"<op> failed: "` + the `Display` text of the error variant.
    #[test]
    fn exact_panic_message_matches_display() {
        let slop = panic_message(|| {
            let buf: [u32; 2] = [0, 0];
            let bytes: &[u8] = cast_slice(&buf);
            let _ = cast_slice::<u8, u32>(&bytes[..6]);
        });
        assert_eq!(
            slop,
            format!(
                "cast_slice failed: {}",
                PodCastError::OutputSliceWouldHaveSlop
            )
        );

        let size = panic_message(|| {
            let _ = cast_slice::<(), u8>(&[()]);
        });
        assert_eq!(
            size,
            format!("cast_slice failed: {}", PodCastError::SizeMismatch)
        );

        let align = panic_message(|| {
            let buf = [0u8; 8];
            let off = if (buf.as_ptr() as usize) % 4 == 3 {
                2
            } else {
                1
            };
            let _ = cast_slice::<u8, u32>(&buf[off..off + 4]);
        });
        assert_eq!(
            align,
            format!(
                "cast_slice failed: {}",
                PodCastError::TargetAlignmentGreaterAndInputNotAligned
            )
        );
    }

    #[test]
    fn empty_slice_cast() {
        let src: [u32; 0] = [];
        let bytes: &[u8] = cast_slice(&src);
        assert!(bytes.is_empty());
        let back: &[f32] = try_cast_slice(bytes).expect("aligned empty slice");
        assert!(back.is_empty());

        let raw = [0u8; 8];
        let off = usize::from((raw.as_ptr() as usize).is_multiple_of(align_of::<u32>()));
        assert_eq!(
            try_cast_slice::<u8, u32>(&raw[off..off]),
            Err(PodCastError::TargetAlignmentGreaterAndInputNotAligned)
        );
    }
}
