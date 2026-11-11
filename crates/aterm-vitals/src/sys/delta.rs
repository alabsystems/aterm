// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `since=<cursor>` deltas (§13.5).

use super::view::{Line, Reply};

/// One reply's line fingerprints: `(key, hash of the displayed text)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fingerprints {
    /// In the reply's order.
    pub lines: Vec<(String, u64)>,
}

/// FNV-1a 64-bit hash, stable across builds: `aterm_hash::fnv1a_64`, spelled out because this
/// crate depends on nothing. `fnv_is_aterm_hashs_fold` holds it to that.
#[must_use]
pub fn fnv(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// The fingerprints of `reply`'s value lines (record and sentence lines).
#[must_use]
pub fn fingerprints(reply: &Reply) -> Fingerprints {
    Fingerprints {
        lines: reply
            .lines
            .iter()
            .filter(|l| l.value)
            .map(|l| (l.key.clone(), fnv(&l.text)))
            .collect(),
    }
}

/// Folds unchanged value lines in `reply` against `base` into one `same <k1>,<k2>,…` line.
pub fn apply_delta(reply: &mut Reply, base: &Fingerprints) -> Vec<String> {
    let mut same = Vec::new();
    let mut first_same: Option<usize> = None;
    let mut kept: Vec<Line> = Vec::with_capacity(reply.lines.len());
    for line in reply.lines.drain(..) {
        let unchanged = line.value
            && base
                .lines
                .iter()
                .any(|(k, h)| *k == line.key && *h == fnv(&line.text));
        if unchanged {
            if first_same.is_none() {
                first_same = Some(kept.len());
            }
            same.push(line.key.clone());
        } else {
            kept.push(line);
        }
    }
    if let Some(at) = first_same {
        let keys: Vec<String> = same.iter().map(|k| k.replace(' ', "_")).collect();
        kept.insert(
            at,
            Line {
                key: "same".into(),
                text: format!("same {}", keys.join(",")),
                value: false,
            },
        );
    }
    reply.lines = kept;
    same
}

#[cfg(test)]
#[path = "delta_tests.rs"]
mod tests;
