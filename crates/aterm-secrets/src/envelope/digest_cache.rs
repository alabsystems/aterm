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
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn equal_envelope_input_avoids_repeating_its_digest() {
        let cache = Cache::new();
        let input = b"equal encrypted envelope input";
        let expected = Sha256::digest(input);
        let first = input.to_vec();
        let second = input.to_vec();
        assert_eq!(cache.digest(&first), expected);
        assert_eq!(cache.digest(&second), expected);
        assert_eq!(cache.hashes.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn envelope_digest_observes_changed_and_restored_input() {
        let cache = Cache::new();
        for input in [
            b"first encrypted input".as_slice(),
            b"first encrypted inpuu".as_slice(),
            b"first encrypted input with a tail".as_slice(),
            b"".as_slice(),
            b"first encrypted input".as_slice(),
        ] {
            assert_eq!(cache.digest(input), Sha256::digest(input));
        }
    }

    #[test]
    fn envelope_digest_retention_is_bounded_by_count_and_input_bytes() {
        let cache = Cache::new();
        for tag in 0..=MAX_ENTRIES {
            cache.digest(&[u8::try_from(tag).unwrap(); 32]);
        }
        assert_eq!(cache.entries.lock().unwrap().len(), MAX_ENTRIES);
        cache.digest(&[0; 32]);
        assert_eq!(cache.hashes.load(Ordering::Relaxed), MAX_ENTRIES + 2);

        let cache = Cache::new();
        let first = vec![b'a'; super::super::MAX_ENVELOPE];
        let second = vec![b'b'; super::super::MAX_ENVELOPE];
        cache.digest(&first);
        cache.digest(&second);
        cache.digest(b"new input");
        {
            let entries = cache.entries.lock().unwrap();
            assert!(
                entries
                    .iter()
                    .map(|entry| entry.bytes.as_slice().len())
                    .sum::<usize>()
                    <= MAX_BYTES
            );
            assert!(entries.iter().all(|entry| entry.bytes.as_slice() != first));
        }
        assert_eq!(cache.digest(&first), Sha256::digest(&first));
        assert_eq!(cache.hashes.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn oversized_input_still_hashes_without_retention() {
        let cache = Cache::new();
        let input = vec![b'x'; super::super::MAX_ENVELOPE + 1];
        let expected = Sha256::digest(&input);
        assert_eq!(cache.digest(&input), expected);
        assert_eq!(cache.digest(&input), expected);
        assert_eq!(cache.hashes.load(Ordering::Relaxed), 2);
        assert!(cache.entries.lock().unwrap().is_empty());
    }
}
