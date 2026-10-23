// Copyright 2026 Andrew Yates
// Copyright (c) 2018-2024 The rust-random Project Developers
// SPDX-License-Identifier: Apache-2.0
//
// Derived from getrandom 0.2.17 (src/error.rs, src/error_impls.rs), offered
// under MIT OR Apache-2.0 and taken here under Apache-2.0. Modified by the
// aterm project in 2026: an OS error's description comes from std's message
// rather than a direct strerror_r(3) call, and `from_io` maps aterm-uds's
// io::Error in.

//! [`Error`]: upstream's codes, descriptions and formatting.

use core::fmt;
use core::num::NonZeroU32;

/// A getrandom failure, as a non-zero code.
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct Error(NonZeroU32);

#[inline]
const fn internal_error(n: u16) -> Error {
    match NonZeroU32::new(Error::INTERNAL_START + n as u32) {
        Some(code) => Error(code),
        None => panic!("INTERNAL_START is non-zero and n cannot wrap it"),
    }
}

impl Error {
    /// This target has no backend.
    pub const UNSUPPORTED: Error = internal_error(0);
    /// The platform's `errno` was not positive.
    pub const ERRNO_NOT_POSITIVE: Error = internal_error(1);
    /// A situation that should not happen in practice.
    pub const UNEXPECTED: Error = internal_error(2);
    /// `CCRandomGenerateBytes` failed on iOS, tvOS or watchOS (unused here).
    pub const IOS_SEC_RANDOM: Error = internal_error(3);
    /// `RtlGenRandom` failed on Windows (unused here).
    pub const WINDOWS_RTL_GEN_RANDOM: Error = internal_error(4);
    /// `RDRAND` failed (unused here).
    pub const FAILED_RDRAND: Error = internal_error(5);
    /// `RDRAND` is not supported on this CPU (unused here).
    pub const NO_RDRAND: Error = internal_error(6);
    /// The Web Crypto API is unavailable (unused here).
    pub const WEB_CRYPTO: Error = internal_error(7);
    /// `crypto.getRandomValues` failed (unused here).
    pub const WEB_GET_RANDOM_VALUES: Error = internal_error(8);
    /// VxWorks `randSecure` reports the RNG uninitialised (unused here).
    pub const VXWORKS_RAND_SECURE: Error = internal_error(11);
    /// Node.js has no `crypto` CommonJS module (unused here).
    pub const NODE_CRYPTO: Error = internal_error(12);
    /// Node.js `crypto.randomFillSync` failed (unused here).
    pub const NODE_RANDOM_FILL_SYNC: Error = internal_error(13);
    /// Called from an ES module on Node.js (unused here).
    pub const NODE_ES_MODULE: Error = internal_error(14);

    /// The first code that is not an OS error number: `1 << 31`.
    pub const INTERNAL_START: u32 = 1 << 31;

    /// The first code free for custom backends.
    pub const CUSTOM_START: u32 = (1 << 31) + (1 << 30);

    /// The OS error number, if this error came from the OS.
    #[inline]
    #[must_use]
    pub fn raw_os_error(self) -> Option<i32> {
        i32::try_from(self.0.get()).ok()
    }

    /// The bare code: an OS error number or one of the reserved ranges.
    #[inline]
    #[must_use]
    pub const fn code(self) -> NonZeroU32 {
        self.0
    }

    /// Maps an `aterm-uds` `io::Error` to an [`Error`].
    #[cfg(any(unix, windows))]
    #[inline]
    pub(crate) fn from_io(err: &std::io::Error) -> Error {
        match err.raw_os_error() {
            Some(errno) => u32::try_from(errno)
                .ok()
                .and_then(NonZeroU32::new)
                .map_or(Error::ERRNO_NOT_POSITIVE, Error),
            None => Error::UNEXPECTED,
        }
    }
}

/// The OS's text for `errno`, as upstream's `strerror_r` reading: none for a
/// number the OS does not know (strerror_r fails exactly there, where std
/// prints its own "Unknown error" placeholder instead).
#[cfg(unix)]
fn os_description(errno: i32) -> Option<String> {
    let text = std::io::Error::from_raw_os_error(errno).to_string();
    let detail = text.strip_suffix(&format!(" (os error {errno})"))?;
    if detail.is_empty() || detail.starts_with("Unknown error") {
        return None;
    }
    Some(detail.to_owned())
}

/// Upstream has no OS text off Unix.
#[cfg(not(unix))]
#[inline]
const fn os_description(_errno: i32) -> Option<String> {
    None
}

#[inline]
const fn internal_desc(error: Error) -> Option<&'static str> {
    match error {
        Error::UNSUPPORTED => Some("getrandom: this target is not supported"),
        Error::ERRNO_NOT_POSITIVE => Some("errno: did not return a positive value"),
        Error::UNEXPECTED => Some("unexpected situation"),
        Error::IOS_SEC_RANDOM => Some("SecRandomCopyBytes: iOS Security framework failure"),
        Error::WINDOWS_RTL_GEN_RANDOM => Some("RtlGenRandom: Windows system function failure"),
        Error::FAILED_RDRAND => Some("RDRAND: failed multiple times: CPU issue likely"),
        Error::NO_RDRAND => Some("RDRAND: instruction not supported"),
        Error::WEB_CRYPTO => Some("Web Crypto API is unavailable"),
        Error::WEB_GET_RANDOM_VALUES => Some("Calling Web API crypto.getRandomValues failed"),
        Error::VXWORKS_RAND_SECURE => Some("randSecure: VxWorks RNG module is not initialized"),
        Error::NODE_CRYPTO => Some("Node.js crypto CommonJS module is unavailable"),
        Error::NODE_RANDOM_FILL_SYNC => Some("Calling Node.js API crypto.randomFillSync failed"),
        Error::NODE_ES_MODULE => Some(
            "Node.js ES modules are not directly supported, see \
             https://docs.rs/getrandom#nodejs-es-module-support",
        ),
        _ => None,
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut dbg = f.debug_struct("Error");
        if let Some(errno) = self.raw_os_error() {
            dbg.field("os_error", &errno);
            if let Some(desc) = os_description(errno) {
                dbg.field("description", &desc);
            }
        } else if let Some(desc) = internal_desc(*self) {
            dbg.field("internal_code", &self.0.get());
            dbg.field("description", &desc);
        } else {
            dbg.field("unknown_code", &self.0.get());
        }
        dbg.finish()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(errno) = self.raw_os_error() {
            match os_description(errno) {
                Some(desc) => fmt::Display::fmt(desc.as_str(), f),
                None => write!(f, "OS Error: {errno}"),
            }
        } else if let Some(desc) = internal_desc(*self) {
            f.write_str(desc)
        } else {
            write!(f, "Unknown Error: {}", self.0.get())
        }
    }
}

impl From<NonZeroU32> for Error {
    #[inline]
    fn from(code: NonZeroU32) -> Self {
        Self(code)
    }
}

impl From<Error> for std::io::Error {
    fn from(err: Error) -> Self {
        match err.raw_os_error() {
            Some(errno) => std::io::Error::from_raw_os_error(errno),
            None => std::io::Error::other(err),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::Error;
    use std::io;

    #[test]
    fn an_aterm_uds_error_maps_to_upstreams_codes() {
        let os = |n| Error::from_io(&io::Error::from_raw_os_error(n));
        assert_eq!(os(1).raw_os_error(), Some(1));
        assert_eq!(os(38).raw_os_error(), Some(38));
        assert_eq!(os(i32::MAX).raw_os_error(), Some(i32::MAX));
        assert_eq!(os(0), Error::ERRNO_NOT_POSITIVE);
        assert_eq!(os(-4), Error::ERRNO_NOT_POSITIVE);
        let eof = io::Error::from(io::ErrorKind::UnexpectedEof);
        assert_eq!(Error::from_io(&eof), Error::UNEXPECTED);
        let text = io::Error::other("BCryptGenRandom failed (NTSTATUS 0xc0000001)");
        assert_eq!(Error::from_io(&text), Error::UNEXPECTED);
    }
}
