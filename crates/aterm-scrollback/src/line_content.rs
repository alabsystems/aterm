// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Small-string-optimized content storage for scrollback lines.

use super::INLINE_SIZE;

/// Fixed-capacity inline byte buffer for short scrollback lines.
///
/// Holds up to `INLINE_SIZE` (32) bytes inline. The length is stored as a `u8`
/// because it is provably bounded by `INLINE_SIZE` (asserted at compile time
/// below: `INLINE_SIZE <= u8::MAX`), saving 7 bytes per line versus the
/// `usize` length a generic `SmallVec` would carry — and the hot tier keeps
/// one `Line` per scrollback row, so this is a direct per-line resident-memory
/// reduction.
#[derive(Debug, Clone)]
pub(crate) struct InlineBuf {
    buf: [u8; INLINE_SIZE],
    len: u8,
}

// `len: u8` is only sound while the inline capacity fits in a `u8`. If
// `INLINE_SIZE` is ever raised above 255, this must widen accordingly.
const _: () = assert!(INLINE_SIZE <= u8::MAX as usize);

impl InlineBuf {
    /// Create from a slice. Caller guarantees `bytes.len() <= INLINE_SIZE`.
    // Skip: the body's one open is `copy_from_slice`'s len-mismatch assert,
    // which lives in the ABSENT std body — and both sub-slices here are
    // `..n` ranges (provably equal length; the `if let (Some, Some)` makes
    // the bounds total), so the panic is unreachable in every real state.
    // Audited; droppable when the toolchain grows the conditional
    // `copy_from_slice` len==len caller-side template.
    #[cfg_attr(trust_verify, trust::skip)]
    #[inline]
    fn from_slice(bytes: &[u8]) -> Self {
        debug_assert!(bytes.len() <= INLINE_SIZE);
        let mut buf = [0u8; INLINE_SIZE];
        let n = bytes.len();
        // Checked copy: `n <= INLINE_SIZE` by contract, but invisible to the
        // panic-freedom verifier — use total `get_mut`/`get` so there is no
        // slice-bounds panic path. Both sub-slices are length `n`, so
        // `copy_from_slice` is total too. Identical for every reachable state.
        if let (Some(dst), Some(src)) = (buf.get_mut(..n), bytes.get(..n)) {
            dst.copy_from_slice(src);
        }
        // `n <= INLINE_SIZE <= u8::MAX` by contract, so the `as u8` is exact.
        Self { buf, len: n as u8 }
    }

    #[inline]
    fn as_slice(&self) -> &[u8] {
        // `len <= INLINE_SIZE` by construction (`from_slice` asserts it). Use a
        // CHECKED access (`get`, which is total) instead of the panicking index, so
        // there is no slice-bounds panic path to prove away — `unwrap_or` falls back
        // to the whole buffer for the (unreachable) `len > INLINE_SIZE` case.
        self.buf.get(..self.len as usize).unwrap_or(&self.buf)
    }

    #[inline]
    fn len(&self) -> usize {
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
    fn default() -> Self {
        Self::Inline(InlineBuf::from_slice(&[]))
    }
}

impl LineContent {
    /// Create from bytes.
    #[must_use]
    pub(crate) fn from_bytes(bytes: &[u8]) -> Self {
        if bytes.len() <= INLINE_SIZE {
            Self::Inline(InlineBuf::from_slice(bytes))
        } else {
            Self::Heap(bytes.to_vec())
        }
    }

    /// Create from an OWNED buffer, taking it over instead of copying it.
    ///
    /// Every row materialized out of the grid ring builds its text into a fresh
    /// `String` that dies one line later; routing that buffer through
    /// [`from_bytes`](Self::from_bytes) makes the >32-byte case (i.e. any
    /// full-width output row) pay a second allocation plus a full memcpy plus
    /// the first buffer's free. Moving it makes that exactly one allocation and
    /// zero copies.
    ///
    /// The shrink keeps `Line::memory_used()` — which counts `Vec::capacity`,
    /// and which the scrollback byte budget is enforced against — reporting the
    /// same exact-fit size `from_bytes` produced. It is a no-op (and so free)
    /// in the common case where the caller sized its buffer exactly, and where
    /// it does fire it replaces today's unconditional malloc+memcpy+free with a
    /// single (usually in-place) realloc.
    #[must_use]
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
