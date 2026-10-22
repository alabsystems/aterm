// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Secret bytes and the process's core-dump switch: what a credential is held as.

use std::sync::atomic::{Ordering, compiler_fence};

/// Bytes that must not outlive their use: scrubbed on drop, never printed. No `Clone`,
/// `Display` or `Serialize`; `Debug` names the length only.
pub struct Secret(Box<[u8]>);

impl Secret {
    /// A secret holding a copy of `bytes`; the caller scrubs its own copy.
    #[must_use]
    pub fn new(bytes: &[u8]) -> Secret {
        Secret(Box::from(bytes))
    }

    /// A secret holding `bytes`. The vector is copied into an exact-size box, then filled
    /// with zeros to its capacity (`resize(capacity, 0)`) and scrubbed whole, so no copy
    /// survives in its spare capacity.
    #[must_use]
    pub fn from_vec(mut bytes: Vec<u8>) -> Secret {
        let kept = Secret(Box::from(bytes.as_slice()));
        // Within the capacity: no reallocation, so the scrub reaches every byte of the
        // vector's allocation, spare capacity included.
        bytes.resize(bytes.capacity(), 0);
        scrub(&mut bytes);
        kept
    }

    /// The bytes, for the one write that needs them.
    #[must_use]
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    /// How many bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
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

/// Overwrite `bytes` with zeros through `std::ptr::write_volatile`, one byte at a time,
/// then `std::sync::atomic::compiler_fence(Ordering::SeqCst)`, so the compiler cannot
/// drop the writes. Each `unsafe` block carries a `// SAFETY:` line: the pointer comes
/// from a live exclusive reference, so it is valid, aligned and unaliased.
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

/// Turn this process's core dumps off (`setrlimit(RLIMIT_CORE, {0, 0})`), so the kernel
/// writes no core file of a crash, and no credential in one. Where Linux pipes core dumps
/// to a handler program (a `core_pattern` that starts with `|`, as systemd-coredump and
/// apport set it), the kernel ignores this limit and the handler decides for itself what
/// it keeps. The result is ignored: a limit already at 0 is the
/// same outcome. A no-op off unix. The call follows atpkg-keys' `forbid_core_dumps`
/// (`master.rs`), including its SAFETY comment.
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

    /// `Debug` names the length and none of the bytes.
    #[test]
    fn debug_names_the_length_and_no_byte() {
        // Two secrets of one length render alike whatever their bytes, so no rendering
        // of a byte (as a character, a number or hex) can be in it; the length changes it.
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

    /// `scrub` zeroes a buffer.
    #[test]
    fn scrub_zeroes_a_buffer() {
        let mut bytes = *b"not zero yet";
        scrub(&mut bytes);
        assert_eq!(bytes, [0; 12]);
    }

    /// `from_vec` keeps the bytes, whatever spare capacity the vector had.
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
