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
#[path = "header_tests.rs"]
mod tests;
