// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::parse_cgi;

#[test]
fn cgi_binary_body_is_exact_and_framing_headers_are_refused() {
    let answer =
        parse_cgi(b"Status: 403 Forbidden\r\nContent-Type: application/octet-stream\r\n\r\n\0\xff")
            .unwrap();
    assert_eq!(answer.status, 403);
    assert_eq!(answer.body, [0, 255]);
    for malformed in [
        b"Content-Length: 9\n\n".as_slice(),
        b"Status: 200 OK\nStatus: 201 Created\n\n",
        b"Content-Type: a\x01b\n\n",
        b"no terminator",
    ] {
        assert!(parse_cgi(malformed).is_err());
    }
}
