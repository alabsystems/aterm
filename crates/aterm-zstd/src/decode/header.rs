// Copyright (c) 2019 Moritz Borcherding
// SPDX-License-Identifier: MIT
//
// Derived from ruzstd 0.8.3 src/decoding/frame.rs, modified by the aterm
// project in 2026. See ../../LICENSE-MIT for the upstream MIT license.

//! The frame header after the magic number (RFC 8878 §3.1.1.1), and the
//! limits it sets for the rest of the frame.

use std::io::{self, Read};

use super::error::DecodeError;
use crate::frame::{BLOCK_MAX, WINDOW_MAX};

const MIN_WINDOW: u64 = 1 << 10;

/// What the header says about the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FrameHeader {
    /// Bytes of history a match may reach; at least [`MIN_WINDOW`], at most
    /// [`WINDOW_MAX`].
    pub(crate) window: usize,
    /// `Block_Maximum_Size`: the smaller of 128 KiB and the window (the
    /// content size itself for a single-segment frame).
    pub(crate) block_max: usize,
    /// `Frame_Content_Size`, when present.
    pub(crate) content_size: Option<u64>,
    /// Whether a `Content_Checksum` follows the last block.
    pub(crate) checksum: bool,
}

/// Reads the header that follows the magic number.
pub(crate) fn read(input: &mut impl Read) -> io::Result<FrameHeader> {
    let mut byte = [0u8; 1];
    input.read_exact(&mut byte)?;
    let [descriptor] = byte;

    let fcs_flag = descriptor >> 6;
    let single_segment = descriptor & 0x20 != 0;
    if descriptor & 0x08 != 0 {
        return Err(DecodeError::ReservedBit.into());
    }
    let checksum = descriptor & 0x04 != 0;
    let dict_id_len = match descriptor & 3 {
        0 => 0,
        1 => 1,
        2 => 2,
        _ => 4,
    };

    let window_descriptor = if single_segment {
        None
    } else {
        input.read_exact(&mut byte)?;
        Some(byte[0])
    };

    if dict_id_len > 0 {
        let mut id = [0u8; 4];
        input.read_exact(&mut id[..dict_id_len])?;
        let id = u32::from_le_bytes(id);
        if id != 0 {
            return Err(DecodeError::Dictionary(id).into());
        }
    }

    let fcs_len = match fcs_flag {
        0 if single_segment => 1,
        0 => 0,
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let content_size = if fcs_len == 0 {
        None
    } else {
        let mut field = [0u8; 8];
        input.read_exact(&mut field[..fcs_len])?;
        let value = u64::from_le_bytes(field);
        Some(if fcs_len == 2 { value + 256 } else { value })
    };

    let declared_window = match (window_descriptor, content_size) {
        (Some(wd), _) => {
            let exponent = u64::from(wd >> 3);
            let mantissa = u64::from(wd & 7);
            let base = 1u64 << (10 + exponent);
            base + (base / 8) * mantissa
        }
        (None, Some(size)) => size,
        (None, None) => 0,
    };
    if declared_window > WINDOW_MAX as u64 {
        return Err(DecodeError::WindowTooLarge(declared_window).into());
    }
    let window = declared_window.max(MIN_WINDOW) as usize;
    let block_max = (declared_window as usize).min(BLOCK_MAX);
    Ok(FrameHeader {
        window,
        block_max,
        content_size,
        checksum,
    })
}

#[cfg(test)]
mod tests {
    use super::{FrameHeader, read};
    use crate::decode::error::DecodeError;

    fn parse(bytes: &[u8]) -> std::io::Result<FrameHeader> {
        read(&mut &bytes[..])
    }

    fn refusal(bytes: &[u8]) -> DecodeError {
        let err = parse(bytes).unwrap_err();
        err.into_inner()
            .unwrap()
            .downcast::<DecodeError>()
            .map(|b| *b)
            .unwrap()
    }

    #[test]
    fn single_segment_window_is_the_content_size() {
        let h = parse(&[0x20, 3]).unwrap();
        assert_eq!(h.content_size, Some(3));
        assert_eq!((h.window, h.block_max), (1024, 3));
        let h = parse(&[0x60, 0xFF, 0xFF]).unwrap();
        assert_eq!(h.content_size, Some(65_791));
        assert_eq!((h.window, h.block_max), (65_791, 65_791));
        let at_cap = (1u32 << 27).to_le_bytes();
        let h = parse(&[0xA0, at_cap[0], at_cap[1], at_cap[2], at_cap[3]]).unwrap();
        assert_eq!((h.window, h.block_max), (1 << 27, 128 * 1024));
        let past = ((1u32 << 27) + 1).to_le_bytes();
        assert_eq!(
            refusal(&[0xA0, past[0], past[1], past[2], past[3]]),
            DecodeError::WindowTooLarge((1 << 27) + 1)
        );
    }

    #[test]
    fn window_descriptor_sizes() {
        let h = parse(&[0x04, 0x00]).unwrap();
        assert_eq!(
            (h.window, h.block_max, h.content_size, h.checksum),
            (1024, 1024, None, true)
        );
        let h = parse(&[0x00, 13 << 3 | 7]).unwrap();
        assert_eq!(h.window, (8 << 20) + 7 * (1 << 20));
        assert_eq!(parse(&[0x00, 17 << 3]).unwrap().window, 1 << 27);
        assert_eq!(
            refusal(&[0x00, 17 << 3 | 1]),
            DecodeError::WindowTooLarge((1 << 27) + (1 << 24))
        );
        assert_eq!(
            refusal(&[0x00, 31 << 3 | 7]),
            DecodeError::WindowTooLarge((1 << 41) + 7 * (1 << 38))
        );
    }

    #[test]
    fn dictionaries_and_the_reserved_bit_are_refused() {
        assert_eq!(refusal(&[0x08, 0x00]), DecodeError::ReservedBit);
        assert_eq!(refusal(&[0x01, 0x00, 7]), DecodeError::Dictionary(7));
        assert_eq!(
            refusal(&[0x03, 0x00, 1, 0, 0, 0x80]),
            DecodeError::Dictionary(0x8000_0001)
        );
        assert!(parse(&[0x02, 0x00, 0, 0]).is_ok());
        assert!(parse(&[0x10, 0x00]).is_ok());
    }

    #[test]
    fn a_short_header_is_an_early_end() {
        for bytes in [
            &[][..],
            &[0x00],
            &[0xA0, 1, 2],
            &[0xC0, 0, 1, 2, 3, 4, 5, 6],
        ] {
            assert_eq!(
                parse(bytes).unwrap_err().kind(),
                std::io::ErrorKind::UnexpectedEof
            );
        }
    }
}
