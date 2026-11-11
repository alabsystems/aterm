// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::io::Read;

use super::*;

fn round_trip(content: &[u8]) -> Vec<u8> {
    let mut br = Vec::new();
    write_uncompressed(&mut br, content).unwrap();
    let mut back = Vec::new();
    crate::Decoder::new(&br[..]).read_to_end(&mut back).unwrap();
    assert_eq!(back, content);
    br
}

/// The writer's streams read back to their content at every meta-block length
/// boundary: empty, one byte, each nibble count's edge, and past one meta-block.
#[test]
fn uncompressed_streams_read_back_at_every_length_boundary() {
    assert_eq!(round_trip(b""), [0b110], "WBITS 16, ISLAST, ISLASTEMPTY");
    assert_eq!(round_trip(b"a").len(), 3 + 1 + 1);
    for len in [0x1_0000, 0x1_0001, 0x10_0000, 0x10_0001, META_BLOCK_MAX + 3] {
        let content: Vec<u8> = (0..len).map(|i| (i * 7 + i / 251) as u8).collect();
        let br = round_trip(&content);
        let blocks = len.div_ceil(META_BLOCK_MAX);
        let header_bytes = br.len() - len;
        assert!(header_bytes <= 1 + 4 * blocks, "{len}: {header_bytes}");
    }
}
