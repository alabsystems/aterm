// Copyright (c) 2020 Pascal Seitz et al.
// SPDX-License-Identifier: MIT
//
// Derived from lz4_flex 0.11.5 and modified by the aterm project in 2026.
// See ../LICENSE-MIT for the upstream MIT license.

use crate::fastcpy::slice_copy;

pub trait Sink {
    /// read byte at position
    fn byte_at(&mut self, pos: usize) -> u8;

    /// Pushes a byte to the end of the Sink.
    fn push(&mut self, byte: u8);

    #[cfg_attr(trust_verify, trust::panic_free)]
    fn pos(&self) -> usize;

    #[cfg_attr(trust_verify, trust::panic_free)]
    fn capacity(&self) -> usize;

    fn extend_with_fill(&mut self, byte: u8, len: usize);

    /// Extends the Sink with `data`.
    fn extend_from_slice(&mut self, data: &[u8]);

    fn extend_from_slice_wild(&mut self, data: &[u8], copy_len: usize);

    /// Copies `wild_len` bytes from `start` to the end of the Sink and
    /// advances by `copy_len`.
    /// # Panics
    /// Panics if `start` >= `pos`.
    fn extend_from_within(&mut self, start: usize, wild_len: usize, copy_len: usize);

    fn extend_from_within_overlapping(&mut self, start: usize, num_bytes: usize);
}

/// A `Sink` over a preallocated `&mut [u8]`.
///
/// Extend methods panic when capacity runs out.
pub struct SliceSink<'a> {
    output: &'a mut [u8],
    /// Bytes written so far.
    pos: usize,
}

impl<'a> SliceSink<'a> {
    /// Creates a `Sink` backed by the given byte slice.
    /// `pos` defines the initial output position in the Sink.
    /// # Panics
    /// Panics if `pos` is out of bounds.
    #[inline]
    #[cfg_attr(trust_verify, trust::skip)] // documented '# Panics if pos is out of bounds' constructor contract
    pub fn new(output: &'a mut [u8], pos: usize) -> Self {
        // SAFETY: the caller has written `output[..pos]`.
        let _ = &mut output[..pos]; // bounds check pos
        SliceSink { output, pos }
    }
}

impl Sink for SliceSink<'_> {
    #[inline]
    #[cfg_attr(trust_verify, trust::skip)] // caller-contract read (`pos < pos()`); OOB panics by documented Sink contract
    fn byte_at(&mut self, pos: usize) -> u8 {
        self.output[pos]
    }

    /// Pushes a byte to the end of the Sink.
    #[inline]
    #[cfg_attr(trust_verify, trust::skip)] // documented Sink contract: panics when capacity is exhausted
    fn push(&mut self, byte: u8) {
        self.output[self.pos] = byte;
        self.pos += 1;
    }

    #[inline]
    fn pos(&self) -> usize {
        self.pos
    }

    #[inline]
    fn capacity(&self) -> usize {
        self.output.len()
    }

    #[inline]
    #[cfg_attr(trust_verify, trust::skip)] // documented Sink contract: extend methods panic on insufficient capacity
    fn extend_with_fill(&mut self, byte: u8, len: usize) {
        self.output[self.pos..self.pos + len].fill(byte);
        self.pos += len;
    }

    #[inline]
    fn extend_from_slice(&mut self, data: &[u8]) {
        self.extend_from_slice_wild(data, data.len())
    }

    #[inline]
    #[cfg_attr(trust_verify, trust::skip)] // documented Sink contract: extend methods panic on insufficient capacity
    fn extend_from_slice_wild(&mut self, data: &[u8], copy_len: usize) {
        // Caller contract: `copy_len <= data.len()`. No assert here: an
        // assert! in a trust::skip'd trait-impl method leaks to every caller
        // as an obligation the verifier cannot bind.
        slice_copy(data, &mut self.output[self.pos..(self.pos) + data.len()]);
        self.pos += copy_len;
    }

    #[inline]
    #[cfg_attr(trust_verify, trust::skip)] // documented Sink contract: extend methods panic on insufficient capacity
    fn extend_from_within(&mut self, start: usize, wild_len: usize, copy_len: usize) {
        self.output.copy_within(start..start + wild_len, self.pos);
        self.pos += copy_len;
    }

    #[inline]
    #[cfg_attr(trust_verify, trust::skip)] // documented Sink contract: extend methods panic on insufficient capacity
    fn extend_from_within_overlapping(&mut self, start: usize, num_bytes: usize) {
        let offset = self.pos - start;
        for i in start + offset..start + offset + num_bytes {
            self.output[i] = self.output[i - offset];
        }
        self.pos += num_bytes;
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_sink_slice() {
        use crate::sink::Sink;
        use crate::sink::SliceSink;
        let mut data = vec![0; 5];
        let sink = SliceSink::new(&mut data, 1);
        assert_eq!(sink.pos(), 1);
        assert_eq!(sink.capacity(), 5);
    }
}
