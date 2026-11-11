// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The window.

use super::error::DecodeError;

/// Bytes a [`BlockBuf`] keeps past the block's maximum for chunked copies to overshoot into.
pub(crate) const SLACK: usize = 32;

const CHUNK: usize = 16;

const OUT_OF_BOUNDS: DecodeError = DecodeError::Corrupt("copy out of bounds");

/// Copies `buf[from..from + 16]` to `buf[to..to + 16]` through a register.
#[inline(always)]
fn copy_chunk(buf: &mut [u8], from: usize, to: usize) -> Result<(), DecodeError> {
    let chunk = *buf
        .get(from..)
        .and_then(<[u8]>::first_chunk::<CHUNK>)
        .ok_or(OUT_OF_BOUNDS)?;
    *buf.get_mut(to..)
        .and_then(<[u8]>::first_chunk_mut::<CHUNK>)
        .ok_or(OUT_OF_BOUNDS)? = chunk;
    Ok(())
}

/// Writes `src[from..from + n]` to `dst[to..]` in 16-byte chunks while both slices hold a whole
/// chunk at the cursor (reading and writing up to 15 bytes past the copy, into room the callers
/// keep).
#[inline(always)]
fn wild_copy(
    dst: &mut [u8],
    to: usize,
    src: &[u8],
    from: usize,
    n: usize,
) -> Result<(), DecodeError> {
    let mut k = 0;
    while k < n {
        let s = src.get(from + k..).and_then(<[u8]>::first_chunk::<CHUNK>);
        let d = dst
            .get_mut(to + k..)
            .and_then(<[u8]>::first_chunk_mut::<CHUNK>);
        if let (Some(s), Some(d)) = (s, d) {
            *d = *s;
            k += CHUNK;
        } else {
            let rest = n - k;
            dst.get_mut(to + k..to + k + rest)
                .ok_or(OUT_OF_BOUNDS)?
                .copy_from_slice(src.get(from + k..from + k + rest).ok_or(OUT_OF_BOUNDS)?);
            return Ok(());
        }
    }
    Ok(())
}

/// The last bytes of the frame's output, up to the window size.
#[derive(Debug, Default)]
pub(crate) struct Window {
    /// Grows to `cap` bytes, then wraps.
    ring: Vec<u8>,
    cap: usize,
    /// Where the next byte goes: `ring.len()` while growing, then the oldest byte's index.
    head: usize,
}

impl Window {
    /// Empties the window for a frame whose window is `cap` bytes (at least one).
    pub(crate) fn reset(&mut self, cap: usize) {
        self.ring.clear();
        self.cap = cap.max(1);
        self.head = 0;
    }

    /// Bytes of history held: everything decoded so far, up to the window.
    pub(crate) const fn len(&self) -> usize {
        self.ring.len()
    }

    /// The frame's maximum match distance.
    pub(crate) const fn max_offset(&self) -> usize {
        self.cap
    }

    /// Appends a block's output, dropping the oldest bytes past the window.
    pub(crate) fn push(&mut self, mut data: &[u8]) {
        if data.len() > self.cap {
            data = data.get(data.len() - self.cap..).unwrap_or_default();
        }
        if self.ring.len() < self.cap {
            let n = data.len().min(self.cap - self.ring.len());
            let (now, rest) = data.split_at_checked(n).unwrap_or((data, &[]));
            if self.ring.capacity() < self.ring.len() + n {
                let target = (self.ring.len() * 2).max(self.ring.len() + n).min(self.cap);
                self.ring.reserve_exact(target - self.ring.len());
            }
            self.ring.extend_from_slice(now);
            self.head = self.ring.len() % self.cap;
            data = rest;
        }
        while !data.is_empty() {
            let n = data.len().min(self.cap - self.head);
            let (now, rest) = data.split_at_checked(n).unwrap_or((data, &[]));
            if let Some(dst) = self.ring.get_mut(self.head..self.head + n) {
                dst.copy_from_slice(now);
            }
            self.head = (self.head + n) % self.cap;
            data = rest;
        }
    }

    fn copy_into(
        &self,
        dst: &mut [u8],
        to: usize,
        back: usize,
        n: usize,
    ) -> Result<(), DecodeError> {
        let len = self.ring.len();
        if back > len || n > back {
            return Err(OUT_OF_BOUNDS);
        }
        let start = if back <= self.head {
            self.head - back
        } else {
            self.head + len - back
        };
        let first = n.min(len - start);
        wild_copy(dst, to, &self.ring, start, first)?;
        if first < n {
            wild_copy(dst, to + first, &self.ring, 0, n - first)?;
        }
        Ok(())
    }
}

/// One block's output: `buf[..len]` is decoded, the rest is room.
#[derive(Debug, Default)]
pub(crate) struct BlockBuf {
    buf: Vec<u8>,
    len: usize,
    /// The block's maximum; `buf` holds at least `max + SLACK` bytes.
    max: usize,
}

impl BlockBuf {
    /// Empties the buffer for a block of at most `max` bytes.
    pub(crate) fn start(&mut self, max: usize) {
        if self.buf.len() < max + SLACK {
            self.buf.resize(max + SLACK, 0);
        }
        self.len = 0;
        self.max = max;
    }

    /// The decoded bytes.
    pub(crate) fn as_slice(&self) -> &[u8] {
        self.buf.get(..self.len).unwrap_or_default()
    }

    pub(crate) const fn len(&self) -> usize {
        self.len
    }

    /// Room for the whole block, to be filled by the caller: the buffer then holds `n` bytes.
    pub(crate) fn fill_with(&mut self, n: usize) -> Result<&mut [u8], DecodeError> {
        if n > self.max {
            return Err(DecodeError::Corrupt("block decodes past its maximum size"));
        }
        self.len = n;
        self.buf.get_mut(..n).ok_or(OUT_OF_BOUNDS)
    }

    /// Whether `n` more bytes fit the block.
    #[inline(always)]
    pub(crate) const fn has_room(&self, n: usize) -> bool {
        n <= self.max - self.len
    }

    /// Appends `lits[from..from + n]`.
    #[inline(always)]
    pub(crate) fn copy_literals(
        &mut self,
        lits: &[u8],
        from: usize,
        n: usize,
    ) -> Result<(), DecodeError> {
        wild_copy(&mut self.buf, self.len, lits, from, n)?;
        self.len += n;
        Ok(())
    }

    /// Appends a match of `n` bytes from `offset` bytes before the end of the output.
    #[inline(always)]
    pub(crate) fn copy_match(
        &mut self,
        history: &Window,
        offset: usize,
        n: usize,
    ) -> Result<(), DecodeError> {
        if offset == 0 {
            return Err(DecodeError::Corrupt("match offset of zero"));
        }
        let mut left = n;
        if offset > self.len {
            let back = offset - self.len;
            if back > history.len() {
                return Err(DecodeError::Corrupt(
                    "match offset reaches before the frame's output",
                ));
            }
            let k = left.min(back);
            history.copy_into(&mut self.buf, self.len, back, k)?;
            self.len += k;
            left -= k;
            if left == 0 {
                return Ok(());
            }
        }
        let dst = self.len;
        let src = dst - offset;
        self.len += left;
        if offset >= CHUNK {
            let mut k = 0;
            while k < left {
                copy_chunk(&mut self.buf, src + k, dst + k)?;
                k += CHUNK;
            }
        } else {
            let first = left.min(CHUNK);
            for i in 0..first {
                let byte = *self.buf.get(src + i).ok_or(OUT_OF_BOUNDS)?;
                *self.buf.get_mut(dst + i).ok_or(OUT_OF_BOUNDS)? = byte;
            }
            let period = offset * CHUNK.div_ceil(offset);
            let mut k = CHUNK;
            while k < left {
                copy_chunk(&mut self.buf, dst + k - period, dst + k)?;
                k += CHUNK;
            }
        }
        Ok(())
    }

    /// Appends `bytes` (a block that is all literals).
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<(), DecodeError> {
        if !self.has_room(bytes.len()) {
            return Err(DecodeError::Corrupt("block decodes past its maximum size"));
        }
        let to = self.len;
        self.buf
            .get_mut(to..to + bytes.len())
            .ok_or(OUT_OF_BOUNDS)?
            .copy_from_slice(bytes);
        self.len += bytes.len();
        Ok(())
    }
}

#[cfg(test)]
#[path = "window_tests.rs"]
mod tests;
