// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the askpass helper's reply codec — behaviour only.

use super::*;

#[test]
fn decode_secret_round_trips() {
    let s = decode_secret("7077").expect("valid hex");
    assert_eq!(s.as_bytes(), b"pw");
}

#[test]
fn decode_secret_empty_is_empty() {
    let s = decode_secret("").expect("empty is valid");
    assert!(s.is_empty());
}

#[test]
fn decode_secret_rejects_malformed_and_line_cutting_bytes() {
    assert!(decode_secret("abc").is_none(), "odd length");
    assert!(decode_secret("zz").is_none(), "non-hex");
    assert!(decode_secret("0a").is_none(), "LF would cut the line");
    assert!(decode_secret("0d").is_none(), "CR would cut the line");
    assert!(decode_secret("00").is_none(), "NUL");
    assert!(
        decode_secret(&"61".repeat(1100)).is_none(),
        "over sudo's 1023-byte read"
    );
}

#[test]
fn an_unknown_reply_never_reaches_the_refusal_diagnostic() {
    let unknown = b"ERR askpass secret=public-synthetic-canary";
    let shown = refusal_reply(unknown);
    assert!(!shown.contains("public-synthetic-canary"), "{shown}");
    assert_eq!(refusal_reply(b"ERR askpass timeout"), "ERR askpass timeout");
}

#[test]
fn sudo_output_is_exactly_one_plain_line_and_empty_writes_nothing() {
    let mut out = Vec::new();
    write_password(&mut out, &Secret::from_bytes(b"public-synthetic")).unwrap();
    assert_eq!(out, b"public-synthetic\n");
    write_password(&mut out, &Secret::from_bytes(b"")).unwrap();
    assert_eq!(out, b"public-synthetic\n");
    assert_eq!(
        refusal_reply(b"ERR askpass refused public-synthetic"),
        "ERR askpass refused"
    );
}

#[cfg(unix)]
#[test]
fn dedicated_reply_transport_accepts_a_complete_line_and_refuses_partial_eof() {
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;
    for (reply, valid) in [
        (b"OK askpass secret=7077\n".as_slice(), true),
        (b"OK askpass secret=7077", false),
    ] {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(reply).unwrap();
        writer.shutdown(Shutdown::Write).unwrap();
        let result = read_answer_within(&reader, Duration::from_secs(60));
        if valid {
            assert_eq!(result.unwrap().as_bytes(), reply);
        } else {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        }
    }
}
