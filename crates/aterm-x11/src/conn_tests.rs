// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Error;

#[test]
fn only_the_stream_error_is_the_transport_failing() {
    assert!(Error::Connection(1).is_transport_failure());
    for code in 2..=7 {
        assert!(!Error::Connection(code).is_transport_failure(), "{code}");
    }
    assert!(!Error::RequestTooLarge.is_transport_failure());
}
