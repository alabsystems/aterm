// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Zstandard frames (RFC 8878 §3.1.1): the writer half.

use std::io::{self, Write};

use crate::xxh64::xxh64;

/// The frame magic number, written little-endian (RFC 8878 §3.1.1).
pub const MAGIC: u32 = 0xFD2F_B528;

/// The largest block a frame may carry.
pub const BLOCK_MAX: usize = 128 * 1024;

/// The largest window a decoder is held to accept.
pub const WINDOW_MAX: usize = 1 << 27;

/// Whether a frame ends with the `Content_Checksum`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checksum {
    /// No checksum: `Content_Checksum_flag` clear.
    Omitted,
    /// The low 32 bits of XXH64 over the content (seed 0), as `zstd` writes by default.
    Xxh64,
}

const SINGLE_SEGMENT: u8 = 1 << 5;
const CONTENT_CHECKSUM: u8 = 1 << 2;
const LAST_BLOCK: u32 = 1;

/// Writes `content` to `out` as one zstd frame of raw blocks.
/// # Errors
/// [`io::ErrorKind::InvalidInput`] when `content` is longer than [`WINDOW_MAX`], before anything is
/// written; otherwise whatever `out` returns. A failed write can leave a partial frame in `out`.
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
        out.write_all(&(xxh64(0, content) as u32).to_le_bytes())?;
    }
    Ok(())
}

enum ContentSize {
    One(u8),
    Two(u16),
    Four(u32),
}

impl ContentSize {
    fn of(len: usize) -> io::Result<Self> {
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
    let word = (size as u32) << 3 | if last { LAST_BLOCK } else { 0 };
    let [a, b, c, _] = word.to_le_bytes();
    [a, b, c]
}

#[cfg(test)]
mod tests {
    use super::{BLOCK_MAX, Checksum, WINDOW_MAX, write_raw};

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
