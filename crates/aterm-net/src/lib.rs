// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! Fleet TLS 1.3 transport, mutual handshake, membership, and peer survey/drive client.

use aterm_codec::hex;

/// TLS 1.3 transport with RFC 5705 keying-material exporter and stream relay.
pub mod tls;

mod slots;

/// Mutual exporter-bound fleet handshake and listener.
pub mod fleet;

/// Fleet membership, peer book, and survey/drive client.
#[cfg(unix)]
pub mod fleet_member;

const MAX_HEX: usize = 4096;

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let len = b.len();
    if len > MAX_HEX {
        return None;
    }
    if !len.is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(len / 2);
    let mut i = 0usize;
    while i < len && i + 1 < len {
        let hi = hex::nibble(b[i])? & 0x0f;
        let lo = hex::nibble(b[i + 1])? & 0x0f;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Some(out)
}
