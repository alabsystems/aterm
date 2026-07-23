// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Inline (stack-allocated) builder for small terminal query replies.
//!
//! Most terminal responses (DA/DSR/cursor-position-report/DECRQSS/…) are a
//! handful of bytes. Building them with `format!`/`String`/`Vec` heap-allocates
//! a throwaway buffer on every query. [`StackResponse`] formats into a fixed
//! `[u8; N]` on the stack instead, so the common small case is allocation-free.
//!
//! The output bytes are identical to the `format!` path: the same
//! `core::fmt::Write` machinery formats the same arguments. If a response ever
//! exceeds `N` bytes the surplus is dropped at the boundary — but the callers
//! here only emit bounded numeric replies (a few `u16` fields plus fixed
//! punctuation) that fit `N` with wide headroom, and a debug assertion guards
//! against silent truncation in test builds.

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
            // Release-build safety net: never write out of bounds. Truncating
            // is preferable to panicking in the hot parser path; callers size
            // `N` so this branch is unreachable in practice.
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
