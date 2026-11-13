// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Digest observations of exact envelope input, independent of its authority.

use aterm_digest::Sha256;
use std::{collections::VecDeque, sync::Mutex};

const MAX_ENTRIES: usize = 16;
const MAX_BYTES: usize = 2 * super::MAX_ENVELOPE;

pub(crate) fn digest(bytes: &[u8]) -> [u8; 32] {
    static CACHE: Cache = Cache::new();
    CACHE.digest(bytes)
}

struct Cache {
    entries: Mutex<VecDeque<Entry>>,
    #[cfg(test)]
    hashes: std::sync::atomic::AtomicUsize,
}

struct Entry {
    bytes: crate::SecretBytes,
    digest: [u8; 32],
}

impl Cache {
    const fn new() -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            #[cfg(test)]
            hashes: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn digest(&self, bytes: &[u8]) -> [u8; 32] {
        // Equality is over the caller's complete current input, never a path,
        // file observation, signer or applied version. A hit grants no rights.
        if let Ok(entries) = self.entries.lock()
            && let Some(entry) = entries.iter().find(|entry| entry.bytes.as_slice() == bytes)
        {
            return entry.digest;
        }
        #[cfg(test)]
        self.hashes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let digest = Sha256::digest(bytes);
        if bytes.len() <= super::MAX_ENVELOPE
            && let Ok(mut entries) = self.entries.lock()
            && !entries.iter().any(|entry| entry.bytes.as_slice() == bytes)
        {
            while entries.len() >= MAX_ENTRIES
                || entries
                    .iter()
                    .map(|entry| entry.bytes.as_slice().len())
                    .sum::<usize>()
                    .saturating_add(bytes.len())
                    > MAX_BYTES
            {
                entries.pop_front();
            }
            entries.push_back(Entry {
                bytes: crate::SecretBytes::new(bytes.to_vec()),
                digest,
            });
        }
        digest
    }
}

#[cfg(test)]
#[path = "digest_cache_tests.rs"]
mod tests;
