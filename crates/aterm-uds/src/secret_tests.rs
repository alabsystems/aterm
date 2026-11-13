// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

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
