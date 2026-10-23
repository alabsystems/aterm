// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/cff/argstack.rs and modified by the aterm
// project in 2026.
// See ../../../LICENSE-MIT and ../../../LICENSE-APACHE for the upstream licenses.

use super::CFFError;

pub struct ArgumentsStack<'a> {
    pub data: &'a mut [f32],
    pub len: usize,
    pub max_len: usize,
}

impl<'a> ArgumentsStack<'a> {
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn push(&mut self, n: f32) -> Result<(), CFFError> {
        if self.len >= self.max_len {
            return Err(CFFError::ArgumentsStackLimitReached);
        }
        let slot = self
            .data
            .get_mut(self.len)
            .ok_or(CFFError::ArgumentsStackLimitReached)?;
        *slot = n;
        self.len += 1;
        Ok(())
    }

    /// The argument at `index` (0 past the buffer).
    #[inline]
    pub fn at(&self, index: usize) -> f32 {
        self.data.get(index).copied().unwrap_or(0.0)
    }

    /// The top argument; 0 on an empty stack.
    #[inline]
    pub fn pop(&mut self) -> f32 {
        let Some(len) = self.len.checked_sub(1) else {
            return 0.0;
        };
        self.len = len;
        self.data.get(len).copied().unwrap_or(0.0)
    }

    /// The argument at `index`, to rewrite in place.
    #[inline]
    pub fn at_mut(&mut self, index: usize) -> Option<&mut f32> {
        self.data.get_mut(index)
    }

    #[inline]
    pub fn reverse(&mut self) {
        if self.is_empty() {
            return;
        }

        if let Some(first) = self.data.get_mut(..self.len) {
            first.reverse();
        }
    }

    #[inline]
    pub fn clear(&mut self) {
        self.len = 0;
    }
}

impl core::fmt::Debug for ArgumentsStack<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_list()
            .entries(self.data.get(..self.len).unwrap_or(&[]))
            .finish()
    }
}
