// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// xxhsum's sanity buffer, the input the recorded XXH64 answers and CLI frames
/// were made from: byte i is the top byte of the generator after i steps
/// (start 2654435761, multiply by 11400714785074694797 mod 2^64).
pub fn sanity(len: usize) -> Vec<u8> {
    let mut g: u64 = 2_654_435_761;
    (0..len)
        .map(|_| {
            let byte = (g >> 56) as u8;
            g = g.wrapping_mul(11_400_714_785_074_694_797);
            byte
        })
        .collect()
}
