// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Copies and verifies `sctk-adwaita` 0.10.1 sources (MIT, Copyright (c) 2022 Bartłomiej Maryńczak) into `OUT_DIR`.

use std::path::{Path, PathBuf};

const VERSION: &str = "sctk-adwaita-0.10.1";

const FILES: [(&str, &str); 4] = [
    (
        "buttons.rs",
        "62f356f085ff947318862ca64817f8b51da84a36e2323ff36956d9fd8429b059",
    ),
    (
        "config.rs",
        "3c1258249f4f3ec8c524a34ea086267fff3be04b58c0b9175a7b34cdf20964f9",
    ),
    (
        "theme.rs",
        "e9838036ee4acc7f2da4c346d8126e57ed53e9be6c4b6ac0df74174b3311fe9a",
    ),
    (
        "shadow.rs",
        "a570bebc85cdd4c8feed81a7979c5eae9a08931d78c762f56b0ab583ad762cd5",
    ),
];

/// Locates the unpacked `sctk-adwaita` crate in the Cargo registry.
fn source() -> PathBuf {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cargo")))
        .expect("CARGO_HOME or HOME");
    let registry = home.join("registry").join("src");
    let found = std::fs::read_dir(&registry)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|index| index.path().join(VERSION))
        .find(|dir| dir.join("src").is_dir());
    found.unwrap_or_else(|| {
        panic!(
            "{VERSION} is not unpacked under {}: run `targo --unverified fetch` here first",
            registry.display()
        )
    })
}

fn main() {
    let dir = source();
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    for (name, want) in FILES {
        let path = dir.join("src").join(name);
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let got = hex(&sha256(text.as_bytes()));
        assert_eq!(
            got,
            want,
            "{} is not the file the recording was made from",
            path.display()
        );
        let body: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("//!"))
            .map(|l| format!("{l}\n"))
            .collect();
        std::fs::write(
            out.join(name),
            format!("use crate::smithay_client_toolkit;\n{body}"),
        )
        .expect("write OUT_DIR");
        println!("cargo:rerun-if-changed={}", path.display());
    }
    std::fs::copy(dir.join("LICENSE"), out.join("LICENSE-sctk-adwaita")).expect("LICENSE");
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// FIPS 180-4 SHA-256.
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for (a, b) in h.iter_mut().zip(v) {
            *a = a.wrapping_add(b);
        }
    }
    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}
