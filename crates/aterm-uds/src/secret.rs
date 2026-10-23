// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Secret bytes and the process's core-dump switch: what a credential is held as.

use std::sync::atomic::{Ordering, compiler_fence};

/// Bytes scrubbed on drop, with `Debug` reporting only the byte length.
pub struct Secret(Box<[u8]>);

impl Secret {
    /// A secret holding a copy of `bytes`; the caller scrubs its own copy.
    #[must_use]
    pub fn new(bytes: &[u8]) -> Secret {
        Secret(Box::from(bytes))
    }

    /// Build a secret from `bytes`, scrubbing its full allocation including spare capacity.
    #[must_use]
    pub fn from_vec(mut bytes: Vec<u8>) -> Secret {
        let kept = Secret(Box::from(bytes.as_slice()));
        bytes.resize(bytes.capacity(), 0);
        scrub(&mut bytes);
        kept
    }

    /// The bytes, for the one write that needs them.
    #[inline]
    #[must_use]
    pub const fn expose(&self) -> &[u8] {
        &self.0
    }

    /// How many bytes.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        scrub(&mut self.0);
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret({} bytes redacted)", self.0.len())
    }
}

/// Overwrite `bytes` with volatile zeroes followed by a `SeqCst` compiler fence.
// Skip: a volatile write is outside what the Trust verifier models.
#[cfg_attr(trust_verify, trust::skip)]
pub fn scrub(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        // SAFETY: the pointer comes from a live exclusive reference, so it is valid,
        // aligned and unaliased.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

/// Disable core dumps for this process via `setrlimit(RLIMIT_CORE, {0, 0})` on Unix.
// Skip: bottoms out at the `setrlimit(2)` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
pub fn no_core_dumps() {
    #[cfg(unix)]
    {
        let zero = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `zero` is a fully initialized `rlimit` living for the call, and
        // `RLIMIT_CORE` is a valid resource. `setrlimit` writes nothing through the pointer.
        unsafe {
            let _ = libc::setrlimit(libc::RLIMIT_CORE, &raw const zero);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Secret, scrub};

    #[test]
    fn debug_names_the_length_and_no_byte() {
        let shown = format!("{:?}", Secret::new(b"zqxwvkjgm"));
        assert_eq!(
            shown,
            format!("{:?}", Secret::new(&[0xff; 9])),
            "the bytes shape nothing"
        );
        assert_ne!(
            shown,
            format!("{:?}", Secret::new(&[0xff; 10])),
            "the length does"
        );
    }

    #[test]
    fn scrub_zeroes_a_buffer() {
        let mut bytes = *b"not zero yet";
        scrub(&mut bytes);
        assert_eq!(bytes, [0; 12]);
    }

    #[test]
    fn from_vec_keeps_the_bytes() {
        let mut bytes = Vec::with_capacity(64);
        bytes.extend_from_slice(b"kept bytes");
        let secret = Secret::from_vec(bytes);
        assert_eq!(secret.expose(), b"kept bytes");
        assert_eq!(secret.len(), 10);
        assert!(!secret.is_empty());
        assert!(Secret::from_vec(Vec::new()).is_empty());
    }

    /// After `no_core_dumps`, `getrlimit(RLIMIT_CORE)` reads 0 for the soft and the hard
    /// limit. The hard limit is what a no-op or a soft-only call leaves above 0, since a
    /// process inherits it unlimited by default.
    #[cfg(unix)]
    #[test]
    fn no_core_dumps_leaves_the_core_limit_at_zero() {
        super::no_core_dumps();
        let mut limit = libc::rlimit {
            rlim_cur: 1,
            rlim_max: 1,
        };
        // SAFETY: `limit` is a fully initialized `rlimit` this frame owns, written by the
        // call and read after it.
        let read = unsafe { libc::getrlimit(libc::RLIMIT_CORE, &raw mut limit) };
        assert_eq!(read, 0);
        assert_eq!(
            (limit.rlim_cur, limit.rlim_max),
            (0, 0),
            "the hard limit too, so nothing raises it again"
        );
    }
}
