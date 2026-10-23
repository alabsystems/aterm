// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `aterm-bits`.

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
/// Implementors must satisfy *every* bullet.
pub unsafe trait Pod: Zeroable + Copy + 'static {}

macro_rules! impl_pod_zeroable_primitive {
    ($($t:ty),* $(,)?) => {
        $(
            // SAFETY: `$t` is a built-in primitive integer/float.

            unsafe impl Zeroable for $t {}
            // SAFETY: see above — `$t` is `Copy + 'static`, has a canonical layout, and every bit pattern is a valid value.

            unsafe impl Pod for $t {}
        )*
    };
}

impl_pod_zeroable_primitive!(u8, u16, u32, u64, u128, usize);
impl_pod_zeroable_primitive!(i8, i16, i32, i64, i128, isize);
impl_pod_zeroable_primitive!(f32, f64);

// SAFETY: `()` is a ZST.

unsafe impl Zeroable for () {}
// SAFETY: see above.

unsafe impl Pod for () {}

// SAFETY: `[T; N]` has layout `T` repeated `N` times with no extra padding (stable guarantee).

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
    /// The source slice is not large enough to cover one target value.
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
/// * If `size_of::<B>() == 0` but `size_of::<A>() != 0` (or vice versa).
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
            // observable through the slice API.
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
        // of `is_null()`: the latter is an unlowerable call boundary.)
        return Ok(&[]);
    }
    // SAFETY: the checks above (all on this path) verified.
    Ok(unsafe { slice::from_raw_parts(ptr_b, out_len) })
}

/// View a single `Pod` value as its raw bytes.
#[inline]
#[must_use]
pub const fn bytes_of<T: Pod>(value: &T) -> &[u8] {
    let ptr = core::ptr::from_ref(value).cast::<u8>();
    if ptr.is_null() {
        // the pointer cast; behavior is unchanged.
        return &[];
    }
    // SAFETY: `ptr` is non-null (guarded above), trivially aligned for `u8`, and points at `value`, so exactly `size_of::<T>()` bytes are reachable.
    unsafe { slice::from_raw_parts(ptr, size_of::<T>()) }
}

/// Wire size in bytes of an encoded POD layout descriptor (`"PODL"`).
pub const POD_LAYOUT_WIRE_BYTES: usize = 16;

/// Wire size in bytes of an encoded POD cast outcome descriptor (`"PODC"`).
pub const POD_CAST_WIRE_BYTES: usize = 16;

/// Pure `const fn` validation of a slice cast's size and alignment parameters.
///
/// # Errors
/// Returns [`PodCastError`] under the exact same conditions as [`try_cast_slice`].
#[inline]
pub const fn validate_pod_cast_params(
    src_byte_len: usize,
    src_addr: usize,
    a_size: usize,
    b_size: usize,
    b_align: usize,
) -> Result<usize, PodCastError> {
    if a_size == 0 || b_size == 0 {
        if a_size == b_size {
            return Ok(0);
        }
        return Err(PodCastError::SizeMismatch);
    }
    if b_align == 0 || !src_addr.is_multiple_of(b_align) {
        return Err(PodCastError::TargetAlignmentGreaterAndInputNotAligned);
    }
    if !src_byte_len.is_multiple_of(b_size) {
        return Err(PodCastError::OutputSliceWouldHaveSlop);
    }
    Ok(src_byte_len / b_size)
}

/// Encode a 16-byte POD layout wire descriptor (`"PODL"` + `elem_size`, `elem_align`, `elem_count` in little-endian `u32`).
#[inline]
#[must_use]
pub const fn encode_pod_layout_wire(
    elem_size: u32,
    elem_align: u32,
    elem_count: u32,
) -> [u8; POD_LAYOUT_WIRE_BYTES] {
    [
        b'P',
        b'O',
        b'D',
        b'L',
        elem_size as u8,
        (elem_size >> 8) as u8,
        (elem_size >> 16) as u8,
        (elem_size >> 24) as u8,
        elem_align as u8,
        (elem_align >> 8) as u8,
        (elem_align >> 16) as u8,
        (elem_align >> 24) as u8,
        elem_count as u8,
        (elem_count >> 8) as u8,
        (elem_count >> 16) as u8,
        (elem_count >> 24) as u8,
    ]
}

/// Decode a 16-byte POD layout wire descriptor produced by [`encode_pod_layout_wire`].
#[inline]
#[must_use]
pub const fn decode_pod_layout_wire(bytes: &[u8]) -> Option<(u32, u32, u32)> {
    if bytes.len() < POD_LAYOUT_WIRE_BYTES {
        return None;
    }
    if bytes[0] != b'P' || bytes[1] != b'O' || bytes[2] != b'D' || bytes[3] != b'L' {
        return None;
    }
    let elem_size = (bytes[4] as u32)
        | ((bytes[5] as u32) << 8)
        | ((bytes[6] as u32) << 16)
        | ((bytes[7] as u32) << 24);
    let elem_align = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let elem_count = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    if elem_align == 0 || (elem_align & (elem_align - 1)) != 0 {
        return None;
    }
    Some((elem_size, elem_align, elem_count))
}

/// Encode a 16-byte POD cast outcome wire descriptor (`"PODC"`).
#[inline]
#[must_use]
pub const fn encode_pod_cast_wire(
    src_byte_len: u32,
    dst_elem_size: u32,
    dst_align: u16,
    err: Option<PodCastError>,
) -> [u8; POD_CAST_WIRE_BYTES] {
    let code = match err {
        None => 0u8,
        Some(PodCastError::TargetAlignmentGreaterAndInputNotAligned) => 1u8,
        Some(PodCastError::OutputSliceWouldHaveSlop) => 2u8,
        Some(PodCastError::SizeMismatch) => 3u8,
    };
    [
        b'P',
        b'O',
        b'D',
        b'C',
        src_byte_len as u8,
        (src_byte_len >> 8) as u8,
        (src_byte_len >> 16) as u8,
        (src_byte_len >> 24) as u8,
        dst_elem_size as u8,
        (dst_elem_size >> 8) as u8,
        (dst_elem_size >> 16) as u8,
        (dst_elem_size >> 24) as u8,
        dst_align as u8,
        (dst_align >> 8) as u8,
        code,
        0,
    ]
}

/// Decode a 16-byte POD cast outcome wire descriptor produced by [`encode_pod_cast_wire`].
#[inline]
#[must_use]
pub const fn decode_pod_cast_wire(bytes: &[u8]) -> Option<(u32, u32, u16, Option<PodCastError>)> {
    if bytes.len() < POD_CAST_WIRE_BYTES {
        return None;
    }
    if bytes[0] != b'P' || bytes[1] != b'O' || bytes[2] != b'D' || bytes[3] != b'C' {
        return None;
    }
    if bytes[15] != 0 {
        return None;
    }
    let err = match bytes[14] {
        0 => None,
        1 => Some(PodCastError::TargetAlignmentGreaterAndInputNotAligned),
        2 => Some(PodCastError::OutputSliceWouldHaveSlop),
        3 => Some(PodCastError::SizeMismatch),
        _ => return None,
    };
    let src_byte_len = (bytes[4] as u32)
        | ((bytes[5] as u32) << 8)
        | ((bytes[6] as u32) << 16)
        | ((bytes[7] as u32) << 24);
    let dst_elem_size = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let dst_align = (bytes[12] as u16) | ((bytes[13] as u16) << 8);
    Some((src_byte_len, dst_elem_size, dst_align, err))
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
        let back: &[f32] = try_cast_slice(bytes).expect("empty slice");
        assert!(back.is_empty());
    }

    #[test]
    fn pod_layout_and_cast_wire_roundtrip() {
        assert_eq!(validate_pod_cast_params(16, 0x1000, 1, 4, 4), Ok(4));
        assert_eq!(
            validate_pod_cast_params(14, 0x1000, 1, 4, 4),
            Err(PodCastError::OutputSliceWouldHaveSlop)
        );
        assert_eq!(
            validate_pod_cast_params(16, 0x1002, 1, 4, 4),
            Err(PodCastError::TargetAlignmentGreaterAndInputNotAligned)
        );
        assert_eq!(
            validate_pod_cast_params(0, 0x1000, 0, 4, 4),
            Err(PodCastError::SizeMismatch)
        );

        let lw = encode_pod_layout_wire(16, 8, 1024);
        assert_eq!(decode_pod_layout_wire(&lw), Some((16, 8, 1024)));
        let bad_align = encode_pod_layout_wire(16, 3, 1024);
        assert!(decode_pod_layout_wire(&bad_align).is_none());

        for err in [
            None,
            Some(PodCastError::TargetAlignmentGreaterAndInputNotAligned),
            Some(PodCastError::OutputSliceWouldHaveSlop),
            Some(PodCastError::SizeMismatch),
        ] {
            let cw = encode_pod_cast_wire(64, 8, 8, err);
            assert_eq!(decode_pod_cast_wire(&cw), Some((64, 8, 8, err)));
        }
    }
}
