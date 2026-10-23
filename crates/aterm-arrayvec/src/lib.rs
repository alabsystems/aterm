// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! First-party `arrayvec` facade ([`ArrayVec`], [`IntoIter`], [`Drain`], [`CapacityError`]).

#![no_std]

pub use aterm_alloc::{
    ArrayVec, ArrayVecDrain as Drain, ArrayVecIntoIter as IntoIter, CapacityError,
};
