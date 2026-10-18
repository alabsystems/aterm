// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux arm's PARSERS — pure functions over `&str` and `&[u8]`,
//! compiled and tested on every platform. A Linux build reads each file
//! through [`crate::read_bounded`] (one 4 KiB buffer) and hands it to these
//! functions, so every parser tolerates a text cut at 4 KiB — a torn last
//! line is ignored, and a fact that was cut off reads `None`.
//!
//! | file | parser | into |
//! |---|---|---|
//! | `/proc/meminfo` | [`meminfo`] | [`crate::physical_memory_mib`], [`crate::machine_memory`] |
//! | `/proc/vmstat` | [`swap_pages`] | [`crate::swapped_kib`] |
//! | `/proc/self/auxv` | [`auxv_value`] | the page size |

/// Complete lines only: a text cut mid-line (at the 4 KiB cap) loses its
/// last, torn line rather than parsing half a number.
fn whole_lines(text: &str) -> impl Iterator<Item = &str> {
    let end = if text.ends_with('\n') {
        text.len()
    } else {
        text.rfind('\n').map_or(0, |i| i + 1)
    };
    text[..end].lines()
}

/// `/proc/meminfo`'s two facts, KiB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemInfo {
    /// `MemTotal`.
    pub total_kib: u64,
    /// `MemAvailable`.
    pub available_kib: u64,
}

/// The value of a `Key:   123 kB` / `key 123` line.
fn keyed(text: &str, key: &str) -> Option<u64> {
    whole_lines(text).find_map(|l| {
        let mut it = l.split_whitespace();
        if it.next()?.trim_end_matches(':') != key {
            return None;
        }
        it.next()?.parse().ok()
    })
}

/// `MemTotal` and `MemAvailable` from `/proc/meminfo`.
#[must_use]
pub fn meminfo(text: &str) -> Option<MemInfo> {
    Some(MemInfo {
        total_kib: keyed(text, "MemTotal")?,
        available_kib: keyed(text, "MemAvailable")?,
    })
}

/// `pswpin + pswpout` from `/proc/vmstat` (pages).
#[must_use]
pub fn swap_pages(vmstat: &str) -> Option<u64> {
    Some(keyed(vmstat, "pswpin")?.saturating_add(keyed(vmstat, "pswpout")?))
}

/// One value of a binary auxiliary vector (`/proc/self/auxv`): native-endian
/// `(key, value)` pairs of the platform's word, ended by `AT_NULL`. `None`
/// when the key is absent or the bytes are cut.
#[must_use]
pub fn auxv_value(bytes: &[u8], key: u64) -> Option<u64> {
    const W: usize = std::mem::size_of::<usize>();
    let word = |b: &[u8]| -> u64 {
        let mut a = [0u8; W];
        a.copy_from_slice(b);
        u64::try_from(usize::from_ne_bytes(a)).unwrap_or(u64::MAX)
    };
    let (pairs, _) = bytes.as_chunks::<{ 2 * W }>();
    pairs.iter().find_map(|pair| {
        let k = word(&pair[..W]);
        (k == key).then(|| word(&pair[W..]))
    })
}
