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
        while self.nbits >= 8 {
            self.out.write_all(&[self.acc as u8])?;
            self.acc >>= 8;
            self.nbits -= 8;
        }
        Ok(())
    }

    /// Zero bits up to the next byte boundary.
    fn pad(&mut self) -> io::Result<()> {
        let n = (8 - self.nbits % 8) % 8;
        self.bits(0, n)
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
mod tests {
    use std::io::Read;

    use super::*;

    fn round_trip(content: &[u8]) -> Vec<u8> {
        let mut br = Vec::new();
        write_uncompressed(&mut br, content).unwrap();
        let mut back = Vec::new();
        crate::Decoder::new(&br[..]).read_to_end(&mut back).unwrap();
        assert_eq!(back, content);
        br
    }

    /// The writer's streams read back to their content at every meta-block length
    /// boundary: empty, one byte, each nibble count's edge, and past one meta-block.
    #[test]
    fn uncompressed_streams_read_back_at_every_length_boundary() {
        assert_eq!(round_trip(b""), [0b110], "WBITS 16, ISLAST, ISLASTEMPTY");
        assert_eq!(round_trip(b"a").len(), 3 + 1 + 1);
        for len in [0x1_0000, 0x1_0001, 0x10_0000, 0x10_0001, META_BLOCK_MAX + 3] {
            let content: Vec<u8> = (0..len).map(|i| (i * 7 + i / 251) as u8).collect();
            let br = round_trip(&content);
            let blocks = len.div_ceil(META_BLOCK_MAX);
            let header_bytes = br.len() - len;
            assert!(header_bytes <= 1 + 4 * blocks, "{len}: {header_bytes}");
        }
    }
}
