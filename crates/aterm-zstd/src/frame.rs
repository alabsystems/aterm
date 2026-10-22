// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Zstandard frames (RFC 8878 §3.1.1): the writer half.
//!
//! [`write_raw`] stores its content instead of compressing it, in the layout
//! libzstd itself emits for input it cannot shrink:
//!
//! ```text
//! Magic_Number            4 bytes  0xFD2FB528, little-endian
//! Frame_Header_Descriptor 1 byte   Single_Segment, Content_Checksum, FCS size
//! Frame_Content_Size      1|2|4    the content length (no Window_Descriptor)
//! Block_Header + bytes    3+n      Raw_Block, n <= BLOCK_MAX; the last flagged
//! Content_Checksum        0|4      low 32 bits of XXH64(content, 0), LE
//! ```
//!
//! A single-segment frame's window is its content size, so a decoder sizes
//! one buffer from the header and every raw block fits it by construction.

use std::io::{self, Write};

use crate::xxh64::xxh64;

/// The frame magic number, written little-endian (RFC 8878 §3.1.1).
pub const MAGIC: u32 = 0xFD2F_B528;

/// The largest block a frame may carry: `Block_Maximum_Size` is the smaller of
/// the window and 128 KiB (RFC 8878 §3.1.1.2.3).
pub const BLOCK_MAX: usize = 128 * 1024;

/// The largest window a decoder is held to accept: 2^27, libzstd's
/// `ZSTD_WINDOWLOG_LIMIT_DEFAULT` and the cap the audit gives aterm's own
/// decoder. A single-segment frame's window is its content size, so
/// [`write_raw`] refuses content past this. (libzstd's streaming decoder
/// refuses windows past 2^27 + 1, `frameParameter_windowTooLarge`, as
/// tests/libzstd.rs showed while libzstd was in the graph; this crate's
/// decoder refuses past 2^27, tests/decode_frames.rs.)
pub const WINDOW_MAX: usize = 1 << 27;

/// Whether a frame ends with the `Content_Checksum`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checksum {
    /// No checksum: `Content_Checksum_flag` clear.
    Omitted,
    /// The low 32 bits of XXH64 over the content (seed 0), as `zstd` writes by
    /// default.
    Xxh64,
}

/// `Frame_Header_Descriptor` bit 5: no `Window_Descriptor`, and the
/// `Frame_Content_Size` field is always present.
const SINGLE_SEGMENT: u8 = 1 << 5;
/// `Frame_Header_Descriptor` bit 2.
const CONTENT_CHECKSUM: u8 = 1 << 2;
/// `Block_Header` bit 0.
const LAST_BLOCK: u32 = 1;

/// Writes `content` to `out` as one zstd frame of raw blocks (layout in the
/// module docs). Any conforming decoder returns exactly `content`.
///
/// # Errors
///
/// [`io::ErrorKind::InvalidInput`] when `content` is longer than
/// [`WINDOW_MAX`], before anything is written; otherwise whatever `out`
/// returns. A failed write can leave a partial frame in `out`.
pub fn write_raw<W: Write + ?Sized>(
    out: &mut W,
    content: &[u8],
    checksum: Checksum,
) -> io::Result<()> {
    let size = ContentSize::of(content.len())?;
    let checksum_flag = match checksum {
        Checksum::Omitted => 0,
        Checksum::Xxh64 => CONTENT_CHECKSUM,
    };
    out.write_all(&MAGIC.to_le_bytes())?;
    out.write_all(&[size.flag() << 6 | SINGLE_SEGMENT | checksum_flag])?;
    size.write(out)?;

    let mut rest = content;
    loop {
        let (block, after) = rest.split_at(rest.len().min(BLOCK_MAX));
        let last = after.is_empty();
        out.write_all(&block_header(block.len(), last))?;
        out.write_all(block)?;
        rest = after;
        if last {
            break;
        }
    }

    if checksum == Checksum::Xxh64 {
        // The low 32 bits, by definition.
        out.write_all(&(xxh64(0, content) as u32).to_le_bytes())?;
    }
    Ok(())
}

/// `Frame_Content_Size` in the narrowest field that holds it (RFC 8878
/// §3.1.1.1.4). Under Single_Segment, flag 0 is a 1-byte field, flag 1 is
/// 2 bytes holding the size less 256, and flag 2 is 4 bytes. [`WINDOW_MAX`]
/// fits 4 bytes, so flag 3 (8 bytes) is never needed.
enum ContentSize {
    One(u8),
    Two(u16),
    Four(u32),
}

impl ContentSize {
    fn of(len: usize) -> io::Result<Self> {
        // Each cast is exact: its arm bounds `len`.
        match len {
            0..=255 => Ok(Self::One(len as u8)),
            256..=65_791 => Ok(Self::Two((len - 256) as u16)),
            _ if len <= WINDOW_MAX => Ok(Self::Four(len as u32)),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "zstd raw frame: {len} bytes of content is past the {WINDOW_MAX}-byte \
                     window a single-segment frame may declare"
                ),
            )),
        }
    }

    /// `Frame_Content_Size_flag`, bits 6-7 of the descriptor.
    #[inline]
    const fn flag(&self) -> u8 {
        match self {
            Self::One(_) => 0,
            Self::Two(_) => 1,
            Self::Four(_) => 2,
        }
    }

    #[inline]
    fn write<W: Write + ?Sized>(&self, out: &mut W) -> io::Result<()> {
        match *self {
            Self::One(n) => out.write_all(&[n]),
            Self::Two(n) => out.write_all(&n.to_le_bytes()),
            Self::Four(n) => out.write_all(&n.to_le_bytes()),
        }
    }
}

/// A `Raw_Block` header (RFC 8878 §3.1.1.2): `Last_Block` in bit 0,
/// `Block_Type` 0 in bits 1-2, `Block_Size` in bits 3-23, little-endian.
#[inline]
const fn block_header(size: usize, last: bool) -> [u8; 3] {
    // size <= BLOCK_MAX = 2^17, so the cast is exact and the shift fits 21 bits.
    let word = (size as u32) << 3 | if last { LAST_BLOCK } else { 0 };
    let [a, b, c, _] = word.to_le_bytes();
    [a, b, c]
}

#[cfg(test)]
mod tests {
    use super::{BLOCK_MAX, Checksum, WINDOW_MAX, write_raw};

    /// Counts what is written and keeps none of it.
    struct Count(usize);

    impl std::io::Write for Count {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 += buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Past the window cap nothing is written; at it the frame is whole. The
    /// content is never read (no checksum, a sink that only counts), so its
    /// zeroed pages are never touched.
    #[test]
    fn content_past_the_window_cap_is_refused() {
        let content = vec![0u8; WINDOW_MAX + 1];
        let mut out = Count(0);
        let err = write_raw(&mut out, &content, Checksum::Omitted).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(out.0, 0);
        write_raw(&mut out, &content[..WINDOW_MAX], Checksum::Omitted).unwrap();
        assert_eq!(out.0, 9 + 3 * (WINDOW_MAX / BLOCK_MAX) + WINDOW_MAX);
    }
}
