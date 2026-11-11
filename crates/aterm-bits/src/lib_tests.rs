// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

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

#[test]
fn zeroed_returns_all_zero_bits() {
    assert_eq!(u64::zeroed(), 0);
    assert_eq!(i128::zeroed(), 0);
    assert_eq!(f32::zeroed().to_bits(), 0);
    assert_eq!(f64::zeroed().to_bits(), 0);
    let arr = <[u32; 3]>::zeroed();
    assert_eq!(bytes_of(&arr), &[0u8; 12]);
    assert!(bytes_of(&<()>::zeroed()).is_empty());
}
