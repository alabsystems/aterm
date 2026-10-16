// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `getrandom` 0.2: a first-party replacement for the crates.io package of the
//! same name (the manifest says why the directory name differs).
//!
//! [`getrandom`] forwards to `aterm_uds::rand::fill`, the workspace's one
//! entropy surface, so the OS CSPRNG under every TLS nonce and ephemeral key
//! is the one aterm's own tokens already use. The crate has no `unsafe`: the
//! FFI stays in aterm-uds.
//!
//! # Consumers
//!
//! ```text
//! ring 0.17.14     every native cell   src/rand.rs:165 getrandom::getrandom(dest),
//!                                      the error mapped to Unspecified
//! astream-aead     `sealed` builds     getrandom::getrandom, and an error's Display
//! ```
//!
//! Neither calls [`getrandom_uninit`], names a feature, or registers a custom
//! backend.
//!
//! # The source, per platform, against upstream 0.2.17
//!
//! ```text
//! macOS     getentropy(2), 256 bytes per call;     upstream reports a failure;
//!           /dev/urandom if it fails               aterm-uds reads the device
//! Linux     getentropy(3) (getrandom(2), flags 0,  upstream calls the syscall
//!           256 bytes per call); /dev/urandom      directly; it falls back only
//!           after /dev/random polls readable       on ENOSYS or EPERM, aterm-uds
//!                                                  on any failure, after the
//!                                                  same poll
//! Windows   BCryptGenRandom, system-preferred RNG  upstream retries a failure
//!                                                  with RtlGenRandom; this
//!                                                  reports it
//! other     Err(UNSUPPORTED) for a non-empty       upstream picks wasi, js,
//!           buffer                                 rdrand or custom, or fails
//!                                                  to compile
//! ```
//!
//! Both sides block until the kernel pool is seeded and never return
//! unseeded bytes; an error is never a partly filled buffer returned as `Ok`.
//!
//! # Divergences
//!
//! Each is deliberate and unreachable from the consumers above.
//!
//! 1. **std, not `no_std`.** aterm-uds is std, and every target aterm builds
//!    has std. `std::error::Error` and `From<Error> for std::io::Error` are
//!    always implemented, as upstream does under its `std` feature.
//! 2. **The features are inert** (the manifest lists what each would do).
//!    There is no `register_custom_getrandom!`: a caller fails to compile.
//! 3. **An error from aterm-uds keeps its errno** ([`Error::raw_os_error`]).
//!    An error without one (a fallback device that ends; a Windows NTSTATUS,
//!    which aterm-uds reports as text) is [`Error::UNEXPECTED`], as upstream
//!    reports an early EOF.
//! 4. **[`getrandom_uninit`] initialises `dest` with zeroes first**, copied
//!    from a zeroed buffer of the same length, so no `unsafe` cast is needed.
//!    The zeroes carry no secret; nothing in aterm's graph calls it.
//!
//! `tests/recorded.rs` holds [`Error`]'s answers to those recorded from
//! upstream 0.2.17; `tests/fill.rs` checks every byte of every length up to
//! 4096 is written, with controls that do not write and must be caught.

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]

use core::mem::MaybeUninit;

mod error;

pub use crate::error::Error;

/// Fill `dest` with bytes from the operating system's CSPRNG.
///
/// An empty `dest` succeeds without asking the OS. The call can block early
/// in boot, until the kernel pool is first seeded.
///
/// # Errors
/// When no OS entropy source answers; `dest`'s contents are then unspecified
/// and must not be used.
#[inline]
pub fn getrandom(dest: &mut [u8]) -> Result<(), Error> {
    if dest.is_empty() {
        return Ok(());
    }
    imp::fill(dest)
}

/// [`getrandom`] into possibly uninitialised memory, returning the same memory
/// as bytes, the same length as `dest`.
///
/// `dest` is initialised (with zeroes) before the OS fills it, so no part of
/// it is ever left uninitialised, whatever the result.
///
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
