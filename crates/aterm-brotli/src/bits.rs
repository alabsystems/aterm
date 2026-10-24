// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The bit reader (RFC 7932 §2): bits are taken least significant first from bytes read
//! in order, out of a buffer refilled from the reader underneath.

use std::io::{self, Read};

use crate::error::{DecodeError, Fault, Step};

/// Input read from the reader at a time.
const INPUT_CHUNK: usize = 64 * 1024;

/// A bit reader over `R`.
pub(crate) struct Bits<R> {
    input: R,
    buf: Box<[u8]>,
    /// `buf[pos..len]` is read from `input` and not yet taken into `acc`.
    pos: usize,
    len: usize,
    /// `input` answered 0.
    eof: bool,
    /// Input bytes moved out of the front of `buf` by earlier refills.
    base: u64,
    /// Taken bits, the next one least significant. Bits at and above `nbits` are either
    /// zero or the low bits of `buf[pos]` (what a word-wide fill overhangs), so they are
    /// never part of an answer and a later fill ORs over them unchanged.
    acc: u64,
    nbits: u32,
}

impl<R: Read> Bits<R> {
    pub(crate) fn new(input: R) -> Self {
        Self {
            input,
            buf: vec![0u8; INPUT_CHUNK].into_boxed_slice(),
            pos: 0,
            len: 0,
            eof: false,
            base: 0,
            acc: 0,
            nbits: 0,
        }
    }

    /// At least 57 bits taken, or every bit the input has left.
    #[inline]
    fn fill(&mut self) -> Step<()> {
        if self.pos + 8 <= self.len {
            self.fill_word();
            return Ok(());
        }
        self.fill_slow()
    }

    #[inline]
    fn fill_word(&mut self) {
        let mut word = [0u8; 8];
        word.copy_from_slice(&self.buf[self.pos..self.pos + 8]);
        // Only called with fewer than 32 bits taken (`need` asks for at most 24).
        self.acc |= u64::from_le_bytes(word) << self.nbits;
        let bytes = (63 - self.nbits) >> 3;
        self.pos += bytes as usize;
        self.nbits += bytes << 3;
    }

    #[cold]
    fn fill_slow(&mut self) -> Step<()> {
        loop {
            if self.pos + 8 <= self.len {
                self.fill_word();
                return Ok(());
            }
            if self.eof {
                while self.nbits <= 56 && self.pos < self.len {
                    self.acc |= u64::from(self.buf[self.pos]) << self.nbits;
                    self.pos += 1;
                    self.nbits += 8;
                }
                return Ok(());
            }
            self.refill()?;
        }
    }

    /// Move what is left to the front of the buffer and read once more into it.
    fn refill(&mut self) -> Step<()> {
        self.base += self.pos as u64;
        self.buf.copy_within(self.pos..self.len, 0);
        self.len -= self.pos;
        self.pos = 0;
        loop {
            match self.input.read(&mut self.buf[self.len..]) {
                Ok(0) => {
                    self.eof = true;
                    return Ok(());
                }
                Ok(n) => {
                    self.len += n.min(self.buf.len() - self.len);
                    return Ok(());
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(Fault::Io(e)),
            }
        }
    }

    /// How many bits of the input have been consumed.
    pub(crate) fn position(&self) -> u64 {
        (self.base + self.pos as u64) * 8 - u64::from(self.nbits)
    }

    /// Take at least `n` bits (`n <= 24`) when the input has them.
    #[inline]
    pub(crate) fn need(&mut self, n: u32) -> Step<()> {
        if self.nbits < n {
            self.fill()?;
        }
        Ok(())
    }

    /// The next 15 bits, unconsumed; past the input's end the missing ones read as zero.
    #[inline]
    pub(crate) fn peek15(&mut self) -> Step<u32> {
        self.need(15)?;
        Ok((self.acc & 0x7fff) as u32)
    }

    /// Drop `n` bits [`Self::peek15`] showed.
    #[inline]
    pub(crate) fn consume(&mut self, n: u32) -> Step<()> {
        if self.nbits < n {
            return Err(Fault::Stream(DecodeError::Truncated));
        }
        self.acc >>= n;
        self.nbits -= n;
        Ok(())
    }

    /// The next `n` bits (`n <= 24`) as a number, the first one least significant.
    #[inline]
    pub(crate) fn read(&mut self, n: u32) -> Step<u32> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = (self.acc & ((1 << n) - 1)) as u32;
        self.consume(n)?;
        Ok(v)
    }

    /// Skip to the next byte boundary. The skipped bits must be zero.
    pub(crate) fn align(&mut self, rule: &'static str) -> Step<()> {
        let skip = self.nbits % 8;
        if self.read(skip)? != 0 {
            return crate::error::corrupt(rule);
        }
        Ok(())
    }

    /// After [`Self::align`]: hand `n` whole bytes to `sink`, in pieces. The input ending
    /// first is [`DecodeError::Truncated`].
    pub(crate) fn take_bytes(&mut self, mut n: usize, sink: &mut dyn FnMut(&[u8])) -> Step<()> {
        while n > 0 && self.nbits >= 8 {
            sink(&[self.acc as u8]);
            self.acc >>= 8;
            self.nbits -= 8;
            n -= 1;
        }
        if n == 0 {
            return Ok(());
        }
        // Every taken bit is gone: what `acc` still holds overhangs `buf[pos]`, which is read
        // from the buffer below.
        self.acc = 0;
        while n > 0 {
            if self.pos == self.len {
                if self.eof {
                    return Err(Fault::Stream(DecodeError::Truncated));
                }
                self.refill()?;
                continue;
            }
            let take = n.min(self.len - self.pos);
            sink(&self.buf[self.pos..self.pos + take]);
            self.pos += take;
            n -= take;
        }
        Ok(())
    }

    /// After [`Self::align`]: whether the input has ended with nothing left in it.
    pub(crate) fn at_end(&mut self) -> Step<bool> {
        if self.nbits > 0 {
            return Ok(false);
        }
        loop {
            if self.pos < self.len {
                return Ok(false);
            }
            if self.eof {
                return Ok(true);
            }
            self.refill()?;
        }
    }
}
