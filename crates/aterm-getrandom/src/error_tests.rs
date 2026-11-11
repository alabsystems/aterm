// Copyright 2026 Andrew Yates
// Copyright (c) 2018-2024 The rust-random Project Developers
// SPDX-License-Identifier: Apache-2.0
//
// Derived from getrandom 0.2.17 (src/error.rs, src/error_impls.rs), offered
// under MIT OR Apache-2.0 and taken here under Apache-2.0. Modified by the
// aterm project in 2026: an OS error's description comes from std's message
// rather than a direct strerror_r(3) call, and `from_io` maps aterm-uds's
// io::Error in.

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
