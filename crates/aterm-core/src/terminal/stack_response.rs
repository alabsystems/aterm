// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Inline (stack-allocated) builder for small terminal query replies.
//!
//! [`StackResponse`] formats DA/DSR/CPR/DECRQSS-sized replies into a fixed
//! `[u8; N]` with `core::fmt::Write`, producing the same bytes as `format!`
//! without a heap allocation. Surplus past `N` is dropped; callers emit bounded
//! numeric replies that fit with headroom, and a debug assertion catches
//! truncation in test builds.

use core::fmt::{self, Write};

/// A fixed-capacity, stack-backed byte builder for terminal replies.
pub(super) struct StackResponse<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackResponse<N> {
    /// Create an empty builder.
    #[inline]
    pub(super) fn new() -> Self {
        Self {
            buf: [0u8; N],
            len: 0,
        }
    }

    /// The bytes written so far.
    #[inline]
    pub(super) fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Append a raw byte slice (for fixed ASCII fragments).
    #[inline]
    pub(super) fn push_bytes(&mut self, bytes: &[u8]) {
        let end = self.len + bytes.len();
        debug_assert!(
            end <= N,
            "StackResponse<{N}> overflow: response exceeded inline capacity",
        );
        if end > N {
            let avail = N - self.len;
            self.buf[self.len..N].copy_from_slice(&bytes[..avail]);
            self.len = N;
            return;
        }
        self.buf[self.len..end].copy_from_slice(bytes);
        self.len = end;
    }
}

impl<const N: usize> Write for StackResponse<N> {
    #[inline]
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let end = self.len + bytes.len();
        // Bounded replies always fit; truncation would be a programming error.
        debug_assert!(
            end <= N,
            "StackResponse<{N}> overflow: response exceeded inline capacity",
        );
        if end > N {
            // Never write out of bounds: truncate rather than panic in the parser
            // hot path. Callers size `N` so this is unreachable.
            let avail = N - self.len;
            self.buf[self.len..N].copy_from_slice(&bytes[..avail]);
            self.len = N;
            return Ok(());
        }
        self.buf[self.len..end].copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }
}
