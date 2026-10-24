// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared helpers: decoding, hashing, a trickling reader and a bit writer for hand-built
//! streams.

use std::io::{self, Read};

/// A whole stream, decoded.
pub(crate) fn decode(br: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    aterm_brotli::Decoder::new(br).read_to_end(&mut out)?;
    Ok(out)
}

/// The SHA-256 of `bytes`, lowercase hex.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    aterm_digest::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A reader handing out at most `step` bytes per read.
pub(crate) struct Trickle<'a> {
    pub(crate) rest: &'a [u8],
    pub(crate) step: usize,
}

impl Read for Trickle<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.rest.len().min(self.step).min(buf.len());
        buf[..n].copy_from_slice(&self.rest[..n]);
        self.rest = &self.rest[n..];
        Ok(n)
    }
}

/// Bits gathered least significant first (RFC 7932 §2), for streams no encoder writes.
#[derive(Default)]
pub(crate) struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    nbits: u32,
}

impl BitWriter {
    pub(crate) fn bits(&mut self, value: u64, n: u32) -> &mut Self {
        assert!(n <= 32 && value >> n == 0, "{value} in {n} bits");
        self.acc |= value << self.nbits;
        self.nbits += n;
        while self.nbits >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.nbits -= 8;
        }
        self
    }

    /// Zero bits to the next byte boundary.
    pub(crate) fn pad(&mut self) -> &mut Self {
        let n = (8 - self.nbits % 8) % 8;
        self.bits(0, n)
    }

    /// Whole bytes, after [`Self::pad`].
    pub(crate) fn bytes(&mut self, bytes: &[u8]) -> &mut Self {
        assert_eq!(self.nbits, 0, "bytes start on a boundary");
        self.out.extend_from_slice(bytes);
        self
    }

    /// A simple prefix code (§3.4) of one symbol over `alphabet`.
    pub(crate) fn simple1(&mut self, symbol: u64, alphabet: u64) -> &mut Self {
        let width = 64 - (alphabet - 1).leading_zeros();
        self.bits(1, 2).bits(0, 2).bits(symbol, width)
    }

    /// A simple prefix code of two symbols, `a` coded `0` and `b` coded `1` when `a < b`.
    pub(crate) fn simple2(&mut self, a: u64, b: u64, alphabet: u64) -> &mut Self {
        let width = 64 - (alphabet - 1).leading_zeros();
        self.bits(1, 2).bits(1, 2).bits(a, width).bits(b, width)
    }

    /// The stream so far, padded.
    pub(crate) fn finish(&mut self) -> Vec<u8> {
        self.pad();
        std::mem::take(&mut self.out)
    }
}
