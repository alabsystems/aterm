// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Small-string-optimized content storage for scrollback lines.

use super::INLINE_SIZE;

/// Fixed-capacity inline byte buffer for short scrollback lines.
#[derive(Debug, Clone)]
pub(crate) struct InlineBuf {
    buf: [u8; INLINE_SIZE],
    len: u8,
}

const _: () = assert!(INLINE_SIZE <= u8::MAX as usize);

impl InlineBuf {
    /// Create from a slice.
    // Skip: the one open is `copy_from_slice`'s len-mismatch assert, in the absent std body.
    #[cfg_attr(trust_verify, trust::skip)]
    #[inline]
    fn from_slice(bytes: &[u8]) -> Self {
        debug_assert!(bytes.len() <= INLINE_SIZE);
        let mut buf = [0u8; INLINE_SIZE];
        let n = bytes.len();
        if let (Some(dst), Some(src)) = (buf.get_mut(..n), bytes.get(..n)) {
            dst.copy_from_slice(src);
        }
        Self { buf, len: n as u8 }
    }

    #[inline]
    fn as_slice(&self) -> &[u8] {
        self.buf.get(..self.len as usize).unwrap_or(&self.buf)
    }

    #[inline]
    const fn len(&self) -> usize {
        self.len as usize
    }
}

/// Line content storage.
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

    /// Create from an owned buffer ([`from_bytes`](Self::from_bytes)).
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
    pub(crate) const fn len(&self) -> usize {
        match self {
            Self::Inline(b) => b.len(),
            Self::Heap(v) => v.len(),
        }
    }

    /// Check if empty.
    #[must_use]
    #[inline]
    pub(crate) const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
