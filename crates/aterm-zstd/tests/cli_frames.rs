// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`frame::write_raw`] against frames the zstd 1.5.7 CLI wrote, pinned by
//! SHA-256. The CLI stores input it cannot shrink as raw blocks, so for
//! xxhsum's sanity buffer `zstd -q -19 -c <file>` (and with `--no-check`)
//! writes byte for byte what `write_raw` does, at every Frame_Content_Size
//! width and across the 128 KiB block boundary.
//!
//! Recorded 2026-10-15 (docs/AUDIT-third-party-2026-10-15.md ZC.3): a scratch
//! crate outside the workspace wrote each prefix to `<len>.bin`, Homebrew's
//! zstd CLI v1.5.7 compressed it, and `shasum -a 256` hashed the output. The
//! same run checked 17 lengths up to 4 MiB with `zstd -t` and `zstd -d | cmp`
//! on `write_raw`'s own frames, and found every CLI frame identical to them.

mod support;

use aterm_digest::Sha256;
use aterm_zstd::frame::{self, Checksum};
use support::sanity;

/// `(len, sha256 of zstd -19, sha256 of zstd -19 --no-check)`.
const CLI: &[(usize, &str, &str)] = &[
    (
        0,
        "f96deff1816083fdff8bc3e46c3fe6ca46a6bb49f4d5a00627616c13237a512c",
        "6fb8543883dafcc2ed0934866b8b3f775231f5b0933add08bf502e9e84944cbd",
    ),
    (
        1,
        "494e839d811f7d7bec248a9d603c126dd7563aa70cc642acb600a46f59b0cbe8",
        "7a4c6310840830c5b9c7f58c5b50fbf6489f91d9dcb538a69e0682c06244defe",
    ),
    (
        255,
        "b1de125a1bf7de51d4a107f16fde4c178228e6856ead03097d939b7b04875a97",
        "a537f689b89489b5b8aaeb3a9ced4bd3efcf67d34381cd1f8d39dd6c9191cee9",
    ),
    (
        256,
        "9fae678ef5b8693ab7cb0be5e2ef03db2dbc0fa1787ff20b1776c012cf5dcc6a",
        "aea7938a6138adae7fb46992d6fc058cfc0e0b42303cdb47a1d62f12e8d5bc72",
    ),
    (
        300,
        "e7706b65cd555b2d655882dc3dc0bf452ad065f296524b23169732e0998a4813",
        "ce13eb553340abd5a1995aa9f96980fef7ffdd2cf4805572141836dac362a7f5",
    ),
    (
        65_791,
        "0b1d7d81a9c4e211a3848e54ff2b91ced0eb2b787c3a645f894ea8e61a6e677f",
        "d016e1124ecb1ed88b361ed971ad04bf40ac7c06277b5b2596229d476098ceec",
    ),
    (
        65_792,
        "e4d1533e063868af66bc7d0a122421f8807ac0c7ea47e11f97221a2e821a30d7",
        "791d80644d4df9be8ade398a373338eb033036702a136ee7aafea04d9daed0d3",
    ),
    (
        131_072,
        "1342bbbff8afb604ee7c37eb50f016646a6b8fea222a6cce68dd008ff45d3b61",
        "1616119bcffcd6adb45c8ac56b0f309e88e51b7c8e4ae677a950c6e1787dfd32",
    ),
    (
        131_073,
        "531b2193458de95a69e890a93db288ad2872f854f1fdabe326fef747d847064e",
        "44ff6f2b4ad5f9a14eee35fd5524c2e39ef18f17f12d462e3a7fda2f805961ff",
    ),
    (
        1_000_003,
        "4f3bafe565de969e0125d19fd9fb494514da198eaff8e6f4ae8b1a6a65063ad9",
        "e3c3894a16ef67c6908a9241ba339076696afc664ef68ff26d329850cc3c92ad",
    ),
];

fn sha256_hex(frame: &[u8]) -> String {
    Sha256::digest(frame)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn write_raw_writes_what_the_cli_wrote() {
    let buf = sanity(CLI.iter().map(|r| r.0).max().expect("rows"));
    for &(len, checked, unchecked) in CLI {
        for (checksum, want) in [(Checksum::Xxh64, checked), (Checksum::Omitted, unchecked)] {
            let mut out = Vec::new();
            frame::write_raw(&mut out, &buf[..len], checksum).expect("write_raw");
            assert_eq!(sha256_hex(&out), want, "len {len} {checksum:?}");
        }
    }
}
