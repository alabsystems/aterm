// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The D-Bus decoder fuzz target (docs/AUDIT-third-party-2026-10-15.md A11Y.3).
//! Every byte it reads may come from any process on the bus.
//!
//! ## Running
//!
//! STOCK EXCEPTION (Trust ships no cargo-fuzz and no ASan runtime): this lane
//! runs on a stock nightly.
//!
//! ```bash
//! cd crates/aterm-dbus
//! cargo +nightly fuzz run decode -- -max_total_time=300 -timeout=10
//! ```
//!
//! Seed the corpus with the recorded messages so runs start past the header:
//! `mkdir -p fuzz/corpus/decode && awk -F'\t' '!/^#/ {print $2}'
//! tests/recorded/zbus-5.16.0-messages.tsv | while read h; do echo "$h" | xxd -r -p >
//! fuzz/corpus/decode/$(echo "$h" | cut -c1-16); done`.
//!
//! ## Properties
//!
//! - Nothing panics: `frame_len`, `Message::decode`, the signature parser and
//!   the value decoder answer every input with a value or an error.
//! - A decoded message re-encodes, and that encoding decodes and re-encodes
//!   to the same bytes. Bytes, not `==`: a NaN double is never equal to
//!   itself (the first run's false alarm, now `tests/mutations.rs`'s
//!   `a_nan_double_round_trips_by_its_bits`).
//! - A value decoded under a signature is canonical: it re-encodes to exactly
//!   the bytes it was read from.
//! - Memory stays linear in the input (about 32 bytes per byte at worst);
//!   libFuzzer's `-rss_limit_mb` (2 GiB by default) fails a run past it.

#![no_main]

use aterm_dbus::message::{self, Message};
use aterm_dbus::{Endian, sig, wire};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Property 1: a message, whole or as a prefix, decodes or errs.
    let _ = message::frame_len(data);
    if let Ok(m) = Message::decode(data) {
        let again = m.encode().expect("a decoded message re-encodes");
        let twice = Message::decode(&again).expect("its encoding decodes");
        assert_eq!(twice.encode().expect("and re-encodes"), again);
    }

    // Property 2: the first byte picks a byte order and an offset, the next
    // `len` bytes a signature, the rest is a body under it.
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
