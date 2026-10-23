// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! First-party `getrandom` 0.2 forwarding to `aterm_uds::rand::fill`.

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]

use core::mem::MaybeUninit;

mod error;

pub use crate::error::Error;

/// Fill `dest` with bytes from the operating system's CSPRNG.
/// # Errors
/// When no OS entropy source answers; `dest`'s contents are then unspecified and must not be used.
#[inline]
pub fn getrandom(dest: &mut [u8]) -> Result<(), Error> {
    if dest.is_empty() {
        return Ok(());
    }
    imp::fill(dest)
}

/// [`getrandom`] into possibly uninitialised memory, returning the same memory as bytes, the same
/// length as `dest`.
/// # Errors
/// As [`getrandom`].
#[inline]
pub fn getrandom_uninit(dest: &mut [MaybeUninit<u8>]) -> Result<&mut [u8], Error> {
    let zeroes = vec![0u8; dest.len()];
    let bytes = dest.write_copy_of_slice(&zeroes);
    getrandom(bytes)?;
    Ok(bytes)
}

#[cfg(any(unix, windows))]
mod imp {
    pub(crate) fn fill(dest: &mut [u8]) -> Result<(), crate::Error> {
        aterm_uds::rand::fill(dest).map_err(|e| crate::Error::from_io(&e))
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    pub(crate) fn fill(_dest: &mut [u8]) -> Result<(), crate::Error> {
        Err(crate::Error::UNSUPPORTED)
    }
}
