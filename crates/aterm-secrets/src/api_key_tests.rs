// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn envelope_round_trip_and_provider_binding() {
    for provider in ["google", "meta"] {
        let encoded = encode(provider, b"synthetic-key").unwrap();
        let key = decode(encoded.expose(), provider).unwrap();
        assert_eq!(key.expose(), b"synthetic-key");
        assert!(!format!("{key:?}").contains("synthetic-key"));
        assert!(decode(encoded.expose(), "another-provider").is_err());
    }
}

#[test]
fn ambiguous_and_unsafe_key_envelopes_are_refused() {
    for extra in [
        r#","schema":1"#,
        r#","kind":"api-key""#,
        r#","provider":"google""#,
        r#","key":"second""#,
        r#","extra":true"#,
    ] {
        let encoded =
            format!(r#"{{"schema":1,"kind":"api-key","provider":"google","key":"first"{extra}}}"#);
        assert!(decode(encoded.as_bytes(), "google").is_err());
    }
    for key in [b"".as_slice(), b"line\nfeed", b"white space", &[0x80]] {
        assert!(encode("google", key).is_err());
    }
    assert!(encode("google", &vec![b'x'; KEY_MAX + 1]).is_err());
    assert!(decode(&vec![b' '; INPUT_MAX + 1], "google").is_err());
    assert!(decode(br#"{"schema":1,"kind":"api-key","key":"first"}"#, "google").is_err());
}
