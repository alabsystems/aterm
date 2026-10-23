// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! D-Bus message and value decoder fuzz target (`STOCK EXCEPTION`: cargo-fuzz requires stock nightly).

#![no_main]

use aterm_dbus::message::{self, Message};
use aterm_dbus::{Endian, sig, wire};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = message::frame_len(data);
    if let Ok(m) = Message::decode(data) {
        let again = m.encode().expect("a decoded message re-encodes");
        let twice = Message::decode(&again).expect("its encoding decodes");
        assert_eq!(twice.encode().expect("and re-encodes"), again);
    }

    let Some((&head, rest)) = data.split_first() else {
        return;
    };
    let endian = if head & 1 == 0 {
        Endian::Little
    } else {
        Endian::Big
    };
    let offset = usize::from(head >> 1 & 7);
    let len = usize::from(head >> 4).min(rest.len());
    let (sig_bytes, body) = rest.split_at(len);
    let Ok(sig_text) = std::str::from_utf8(sig_bytes) else {
        return;
    };
    let Ok(types) = sig::parse_signature(sig_text) else {
        return;
    };
    if let Ok(values) = wire::decode(body, &types, endian, offset) {
        let again = wire::encode(&values, endian, offset).expect("a decoded value re-encodes");
        assert_eq!(again, body, "a non-canonical encoding was accepted");
    }
});
