// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Small-string-optimized content storage for scrollback lines.

use super::INLINE_SIZE;

/// Fixed-capacity inline byte buffer for short scrollback lines.
///
/// Holds up to `INLINE_SIZE` (32) bytes inline. The length is a `u8` (bounded by
/// `INLINE_SIZE`, asserted below), saving 7 bytes on every hot-tier `Line`
/// against a `usize` length.
#[derive(Debug, Clone)]
pub(crate) struct InlineBuf {
    buf: [u8; INLINE_SIZE],
    len: u8,
}

// `len: u8` holds only while the inline capacity fits in a `u8`.
const _: () = assert!(INLINE_SIZE <= u8::MAX as usize);

impl InlineBuf {
    /// Create from a slice. Caller guarantees `bytes.len() <= INLINE_SIZE`.
    // Skip: the one open is `copy_from_slice`'s len-mismatch assert, in the
    // absent std body; both sub-slices are `..n`, so it cannot fire. Drop the
    // skip once Trust has a conditional `copy_from_slice` len==len template.
    #[cfg_attr(trust_verify, trust::skip)]
    #[inline]
    fn from_slice(bytes: &[u8]) -> Self {
        debug_assert!(bytes.len() <= INLINE_SIZE);
        let mut buf = [0u8; INLINE_SIZE];
        let n = bytes.len();
        // Total `get_mut`/`get`: `n <= INLINE_SIZE` by contract, but the
        // panic-freedom verifier cannot see it.
        if let (Some(dst), Some(src)) = (buf.get_mut(..n), bytes.get(..n)) {
            dst.copy_from_slice(src);
        }
        // `n <= INLINE_SIZE <= u8::MAX` by contract, so the `as u8` is exact.
        Self { buf, len: n as u8 }
    }

    #[inline]
    fn as_slice(&self) -> &[u8] {
        // Checked `get`: `len <= INLINE_SIZE` by construction, so the fallback is
        // unreachable and there is no slice-bounds panic path to prove away.
        self.buf.get(..self.len as usize).unwrap_or(&self.buf)
    }

    #[inline]
    const fn len(&self) -> usize {
        self.len as usize
    }
}

/// Line content storage.
///
/// Uses small-string optimization: lines up to `INLINE_SIZE` bytes are stored
/// inline, longer lines use heap allocation.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // Inline variant is intentionally large — that's the point of inline storage
pub(crate) enum LineContent {
    /// Inline storage for short lines.
    Inline(InlineBuf),
    /// Heap storage for long lines.
    Heap(Vec<u8>),
}

impl Default for LineContent {
    #[inline]
    fn default() -> Self {
        Self::Inline(InlineBuf::from_slice(&[]))
    }
}

impl LineContent {
    /// Create from bytes.
    #[must_use]
    #[inline]
    pub(crate) fn from_bytes(bytes: &[u8]) -> Self {
        if bytes.len() <= INLINE_SIZE {
            Self::Inline(InlineBuf::from_slice(bytes))
        } else {
            Self::Heap(bytes.to_vec())
        }
    }

    /// Create from an owned buffer, taking it over instead of copying it.
    ///
    /// A row materialized out of the grid builds its text into a fresh `String`;
    /// moving it saves the second allocation and copy that
    /// [`from_bytes`](Self::from_bytes) makes for any line over 32 bytes. The
    /// shrink keeps `Line::memory_used()` (which counts `Vec::capacity` and backs
    /// the byte budget) at the exact-fit size `from_bytes` produces; it is a
    /// no-op when the caller sized the buffer exactly.
    #[must_use]
    #[inline]
    pub(crate) fn from_vec(mut bytes: Vec<u8>) -> Self {
        if bytes.len() <= INLINE_SIZE {
            Self::Inline(InlineBuf::from_slice(&bytes))
        } else {
            if bytes.capacity() != bytes.len() {
                bytes.shrink_to_fit();
            }
            Self::Heap(bytes)
        }
    }

    /// Get as byte slice.
    #[must_use]
    #[inline]
    pub(crate) fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Inline(b) => b.as_slice(),
            Self::Heap(v) => v.as_slice(),
        }
    }

    /// Get the length in bytes.
    #[must_use]
    #[inline]
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Inline(b) => b.len(),
            Self::Heap(v) => v.len(),
        }
    }

    /// Check if empty.
    #[must_use]
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
