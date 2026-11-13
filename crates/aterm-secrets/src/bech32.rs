// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Zeroizing Bech32 (BIP 173) codec for age recipients and identities.

use zeroize::Zeroize;

/// The 32 data characters, by value.
const CHARSET: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

/// The BCH generator (BIP 173 `bech32_polymod`).
const GENERATOR: [u32; 5] = [
    0x3b6a_57b2,
    0x2650_8e6d,
    0x1ea1_19fa,
    0x3d42_33dd,
    0x2a14_62b3,
];

/// What the checksum of a valid Bech32 string evaluates to (bech32m's is 0x2bc830a3).
const CHECKSUM_CONSTANT: u32 = 1;

/// A decoded Bech32 string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    /// The human-readable part, lowercased.
    pub hrp: String,
    /// The data, regrouped from 5-bit groups to bytes (padding bits must be zero).
    pub data: Vec<u8>,
    /// True when the string was written all-uppercase.
    pub uppercase: bool,
}

/// Encode `data` under lowercase ASCII `hrp`.
pub fn encode(hrp: &str, data: &[u8]) -> String {
    let mut groups = to_groups(data);
    let text = encode_groups(hrp, &groups);
    groups.zeroize();
    text
}

/// Decode a Bech32 string.
pub fn decode(text: &str) -> Option<Decoded> {
    let (hrp, mut groups, uppercase) = decode_groups(text)?;
    let data = from_groups(&groups);
    groups.zeroize();
    Some(Decoded {
        hrp,
        data: data?,
        uppercase,
    })
}

fn polymod(values: impl IntoIterator<Item = u8>) -> u32 {
    let mut chk: u32 = 1;
    for value in values {
        let top = chk >> 25;
        chk = ((chk & 0x01ff_ffff) << 5) ^ u32::from(value);
        for (i, generator) in GENERATOR.iter().enumerate() {
            if (top >> i) & 1 == 1 {
                chk ^= generator;
            }
        }
    }
    chk
}

/// BIP 173 `bech32_hrp_expand`, chained with the data groups.
fn checksum_input<'a>(hrp: &'a [u8], groups: &'a [u8]) -> impl Iterator<Item = u8> + 'a {
    hrp.iter()
        .map(|c| c >> 5)
        .chain(std::iter::once(0))
        .chain(hrp.iter().map(|c| c & 31))
        .chain(groups.iter().copied())
}

/// The string for `hrp` and 5-bit `groups` (each below 32), checksum appended.
pub(crate) fn encode_groups(hrp: &str, groups: &[u8]) -> String {
    let hrp = hrp.as_bytes();
    let checksum = polymod(checksum_input(hrp, groups).chain([0u8; 6])) ^ CHECKSUM_CONSTANT;
    let mut text = String::with_capacity(hrp.len() + 1 + groups.len() + 6);
    text.extend(hrp.iter().map(|&c| char::from(c)));
    text.push('1');
    for &group in groups {
        text.push(char::from(CHARSET[usize::from(group & 31)]));
    }
    for i in 0..6 {
        let group = (checksum >> (5 * (5 - i))) & 31;
        text.push(char::from(CHARSET[group as usize]));
    }
    text
}

/// The lowercased HRP, the 5-bit data groups (checksum removed) and whether `text` was uppercase.
pub(crate) fn decode_groups(text: &str) -> Option<(String, Vec<u8>, bool)> {
    let bytes = text.as_bytes();
    if bytes.iter().any(|&c| !(33..=126).contains(&c)) {
        return None;
    }
    let has_lower = bytes.iter().any(u8::is_ascii_lowercase);
    let has_upper = bytes.iter().any(u8::is_ascii_uppercase);
    if has_lower && has_upper {
        return None;
    }
    let separator = bytes.iter().rposition(|&c| c == b'1')?;
    if separator < 1 || separator + 7 > bytes.len() {
        return None;
    }
    let mut lower = text.to_ascii_lowercase();
    let result = (|| {
        let (hrp, rest) = lower.as_bytes().split_at(separator);
        let mut groups = Vec::with_capacity(rest.len() - 1);
        for &c in &rest[1..] {
            match CHARSET.iter().position(|&d| d == c) {
                Some(value) => groups.push(value as u8),
                None => {
                    groups.zeroize();
                    return None;
                }
            }
        }
        if polymod(checksum_input(hrp, &groups)) != CHECKSUM_CONSTANT {
            groups.zeroize();
            return None;
        }
        groups.truncate(groups.len() - 6);
        Some((String::from_utf8_lossy(hrp).into_owned(), groups, has_upper))
    })();
    lower.zeroize();
    result
}

/// Bytes regrouped into 5-bit groups, the last one zero-padded.
fn to_groups(data: &[u8]) -> Vec<u8> {
    let mut groups = Vec::with_capacity((data.len() * 8).div_ceil(5));
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &byte in data {
        acc = ((acc << 8) | u32::from(byte)) & 0xfff;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            groups.push(((acc >> bits) & 31) as u8);
        }
    }
    if bits > 0 {
        groups.push(((acc << (5 - bits)) & 31) as u8);
    }
    groups
}

/// 5-bit groups regrouped into bytes.
fn from_groups(groups: &[u8]) -> Option<Vec<u8>> {
    let mut data = Vec::with_capacity(groups.len() * 5 / 8);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &group in groups {
        acc = ((acc << 5) | u32::from(group & 31)) & 0x1fff;
        bits += 5;
        while bits >= 8 {
            bits -= 8;
            data.push(((acc >> bits) & 0xff) as u8);
        }
    }
    let padding_clean = bits < 5 && (acc << (8 - bits)) & 0xff == 0;
    if padding_clean {
        Some(data)
    } else {
        data.zeroize();
        None
    }
}

#[cfg(test)]
#[path = "bech32_tests.rs"]
mod tests;
