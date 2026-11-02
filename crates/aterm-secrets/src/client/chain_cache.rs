// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Public signer lineage derived from the exact current vault envelope.

use super::{Paths, SecretBytes, envelope, fsx};
use std::{collections::VecDeque, sync::Mutex};

const MAX_CHAINS: usize = 16;
const MAX_CACHED_BYTES: usize = 2 * envelope::MAX_ENVELOPE;

pub(super) fn read(paths: &Paths, pin: &[u8; 32]) -> Vec<[u8; 32]> {
    static CACHE: Cache = Cache::new();
    CACHE.read(paths, pin)
}

struct Cache {
    entries: Mutex<VecDeque<Verified>>,
    #[cfg(test)]
    validations: std::sync::atomic::AtomicUsize,
}

struct Verified {
    pin: [u8; 32],
    bytes: SecretBytes,
    chain: Vec<[u8; 32]>,
}

impl Cache {
    const fn new() -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            #[cfg(test)]
            validations: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn read(&self, paths: &Paths, pin: &[u8; 32]) -> Vec<[u8; 32]> {
        let Some(bytes) = fsx::read_private(&paths.cache_file(), envelope::MAX_ENVELOPE)
            .ok()
            .flatten()
        else {
            return Vec::new();
        };
        // The private file is read afresh before every lookup. Only an exact
        // byte/pin match reuses deterministic public lineage verification.
        if let Ok(entries) = self.entries.lock()
            && let Some(entry) = entries
                .iter()
                .find(|entry| entry.pin == *pin && entry.bytes.as_slice() == bytes.as_slice())
        {
            return entry.chain.clone();
        }
        #[cfg(test)]
        self.validations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let Some(chain) = envelope::parse(bytes.as_slice())
            .ok()
            .filter(|cached| cached.signer == *pin && cached.verify_signature().is_ok())
            .map(|cached| cached.reached_from())
        else {
            return Vec::new();
        };
        if let Ok(mut entries) = self.entries.lock()
            && !entries
                .iter()
                .any(|entry| entry.pin == *pin && entry.bytes.as_slice() == bytes.as_slice())
        {
            // Retain at most sixteen inputs and two maximum-size envelopes'
            // bytes. Expensive verification never holds the cache mutex.
            while entries.len() >= MAX_CHAINS
                || entries
                    .iter()
                    .map(|entry| entry.bytes.as_slice().len())
                    .sum::<usize>()
                    .saturating_add(bytes.as_slice().len())
                    > MAX_CACHED_BYTES
            {
                let _ = entries.pop_front();
            }
            entries.push_back(Verified {
                pin: *pin,
                bytes,
                chain: chain.clone(),
            });
        }
        chain
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    const SIGNER: [u8; 32] = [61; 32];
    const PREVIOUS: [u8; 32] = [33; 32];
    const OTHER: [u8; 32] = [47; 32];

    fn signed(previous: &[u8; 32], sequence: u64) -> Vec<u8> {
        let signer = envelope::signer_public(&SIGNER).unwrap();
        envelope::sign(
            &envelope::Unsigned {
                name: "chain-cache".into(),
                seq: sequence,
                time: 1_792_530_000,
                handoff: vec![(
                    envelope::signer_public(previous).unwrap(),
                    envelope::endorse(previous, "chain-cache", &signer).unwrap(),
                )],
                parents: Vec::new(),
                lockers: vec![envelope::Locker {
                    label: "admin".into(),
                    armored: envelope::test_armor(b"inert encrypted fixture"),
                }],
            },
            &SIGNER,
        )
        .unwrap()
    }

    #[test]
    fn unchanged_current_envelope_reuses_its_verified_public_chain() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(home.path());
        fsx::ensure_private_dir(&paths.dir).unwrap();
        fsx::write_atomic(&paths.cache_file(), &signed(&PREVIOUS, 11), 0o600).unwrap();
        let cache = Cache::new();
        let pin = envelope::signer_public(&SIGNER).unwrap();
        let expected = vec![envelope::signer_public(&PREVIOUS).unwrap()];
        assert_eq!(cache.read(&paths, &pin), expected);
        assert_eq!(cache.validations.load(Ordering::Relaxed), 1);
        assert_eq!(cache.read(&paths, &pin), expected);
        assert_eq!(cache.validations.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn warmed_chain_still_reads_current_bytes_and_checks_the_supplied_pin() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(home.path());
        fsx::ensure_private_dir(&paths.dir).unwrap();
        let first = signed(&PREVIOUS, 11);
        fsx::write_atomic(&paths.cache_file(), &first, 0o600).unwrap();
        let cache = Cache::new();
        let pin = envelope::signer_public(&SIGNER).unwrap();
        let expected = vec![envelope::signer_public(&PREVIOUS).unwrap()];
        assert_eq!(cache.read(&paths, &pin), expected);
        assert!(
            cache
                .read(&paths, &envelope::signer_public(&OTHER).unwrap())
                .is_empty()
        );

        let second = signed(&OTHER, 12);
        fsx::write_atomic(&paths.cache_file(), &second, 0o600).unwrap();
        assert_eq!(
            cache.read(&paths, &pin),
            vec![envelope::signer_public(&OTHER).unwrap()]
        );
        let damaged = String::from_utf8(second)
            .unwrap()
            .replace("seq: 12\n", "seq: 13\n");
        fsx::write_atomic(&paths.cache_file(), damaged.as_bytes(), 0o600).unwrap();
        assert!(cache.read(&paths, &pin).is_empty());
        fsx::write_atomic(&paths.cache_file(), b"not an envelope", 0o600).unwrap();
        assert!(cache.read(&paths, &pin).is_empty());

        fsx::write_atomic(&paths.cache_file(), &first, 0o600).unwrap();
        assert_eq!(cache.read(&paths, &pin), expected);
        std::fs::rename(paths.cache_file(), paths.dir.join("held-vault")).unwrap();
        assert!(cache.read(&paths, &pin).is_empty());
    }
}
