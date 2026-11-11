// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The writer half: a stream of uncompressed meta-blocks (RFC 7932 §9.2).

use std::io::{self, Write};

/// The longest meta-block (MLEN is at most 2^24).
pub const META_BLOCK_MAX: usize = 1 << 24;

/// Bits gathered least significant first, written out a byte at a time.
struct BitWriter<'w, W: ?Sized> {
    out: &'w mut W,
    acc: u64,
    nbits: u32,
}

impl<W: Write + ?Sized> BitWriter<'_, W> {
    fn bits(&mut self, value: u64, n: u32) -> io::Result<()> {
        self.acc |= value << self.nbits;
        self.nbits += n;
        if self.nbits >= 32 {
            self.flush_whole()?;
        }
        Ok(())
    }

    fn flush_whole(&mut self) -> io::Result<()> {
        let whole = (self.nbits / 8) as usize;
        if whole > 0 {
            let bytes = self.acc.to_le_bytes();
            self.out.write_all(&bytes[..whole])?;
            let shift = (whole as u32) * 8;
            self.acc = self.acc.checked_shr(shift).unwrap_or(0);
            self.nbits -= shift;
        }
        Ok(())
    }

    /// Zero bits up to the next byte boundary.
    fn pad(&mut self) -> io::Result<()> {
        let n = (8 - self.nbits % 8) % 8;
        self.nbits += n;
        self.flush_whole()
    }
}

/// Writes `content` to `out` as one Brotli stream of uncompressed meta-blocks, each at
/// most [`META_BLOCK_MAX`] bytes, with a 16-bit window: a valid stream any RFC 7932
/// decoder reads back to `content`, compressing nothing.
///
/// # Errors
/// Whatever `out` returns; a failed write can leave a partial stream in `out`.
pub fn write_uncompressed<W: Write + ?Sized>(out: &mut W, content: &[u8]) -> io::Result<()> {
    let mut w = BitWriter {
        out,
        acc: 0,
        nbits: 0,
    };
    // WBITS 16: a single 0 bit.
    w.bits(0, 1)?;
    for block in content.chunks(META_BLOCK_MAX) {
        let mlen = (block.len() - 1) as u64;
        // The fewest nibbles that hold MLEN - 1, so the last one is never zero past four.
        let nibbles = match mlen {
            0..=0xffff => 4,
            0x1_0000..=0xf_ffff => 5,
            _ => 6,
        };
        w.bits(0, 1)?; // ISLAST
        w.bits(nibbles - 4, 2)?; // MNIBBLES
        w.bits(mlen, 4 * nibbles as u32)?;
        w.bits(1, 1)?; // ISUNCOMPRESSED
        w.pad()?;
        w.out.write_all(block)?;
    }
    w.bits(1, 1)?; // ISLAST
    w.bits(1, 1)?; // ISLASTEMPTY
    w.pad()
}

#[cfg(test)]
#[path = "write_tests.rs"]
mod tests;
