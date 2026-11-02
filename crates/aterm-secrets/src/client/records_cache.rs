// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Recovered membership records under the current vault and admitted key set.

use super::{Paths, fsx, sha256};
use crate::{
    SecretBytes,
    membership_log::{MAX_RECORDS_STREAM, Store},
};
use std::{collections::VecDeque, sync::Mutex};

const MAX_ENTRIES: usize = 16;
const MAX_INPUT_BYTES: usize = 1024 * 1024;

pub(super) fn read(paths: &Paths, vault: String, store: Store) -> crate::Result<Store> {
    static CACHE: Cache = Cache::new();
    CACHE.read(paths, vault, store)
}

struct Cache {
    entries: Mutex<VecDeque<Entry>>,
    #[cfg(test)]
    hashes: std::sync::atomic::AtomicUsize,
}

struct Entry {
    vault: String,
    keys: Option<Vec<[u8; 32]>>,
    digest: [u8; 32],
    bytes: Option<SecretBytes>,
    store: Store,
}

impl Cache {
    const fn new() -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            #[cfg(test)]
            hashes: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn read(&self, paths: &Paths, vault: String, store: Store) -> crate::Result<Store> {
        let Some(bytes) = fsx::read_owned_head(&paths.records_file(), MAX_RECORDS_STREAM)? else {
            return Ok(store);
        };
        let keys = store.keys().map(<[[u8; 32]]>::to_vec);
        let same_context = |entry: &Entry| entry.vault == vault && entry.keys == keys;
        // A recovered store roots another genesis under different admitted
        // keys. Reuse only after reading the current bytes and key context.
        if let Ok(entries) = self.entries.lock()
            && let Some(entry) = entries.iter().find(|entry| {
                same_context(entry)
                    && entry
                        .bytes
                        .as_ref()
                        .is_some_and(|saved| saved.as_slice() == bytes)
            })
        {
            return Ok(entry.store.clone());
        }
        #[cfg(test)]
        self.hashes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let digest = sha256(&bytes);
        let same = |entry: &Entry| same_context(entry) && entry.digest == digest;
        if let Ok(entries) = self.entries.lock()
            && let Some(entry) = entries.iter().find(|entry| same(entry))
        {
            return Ok(entry.store.clone());
        }
        let store = store.recovering(&bytes);
        if let Ok(mut entries) = self.entries.lock() {
            entries.retain(|entry| !same(entry));
            if entries.len() >= MAX_ENTRIES {
                entries.pop_front();
            }
            entries.push_back(Entry {
                vault,
                keys,
                digest,
                bytes: (bytes.len() <= MAX_INPUT_BYTES).then(|| SecretBytes::new(bytes)),
                store: store.clone(),
            });
        }
        Ok(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        age::Identity,
        commons::binding::Request,
        envelope,
        membership_log::{Entrant, Record, encode_records},
    };
    use std::sync::atomic::Ordering;

    const SIGNER: [u8; 32] = [61; 32];
    const OTHER: [u8; 32] = [47; 32];

    fn signed(seed: &[u8; 32], tag: u8) -> Vec<u8> {
        let identity = Identity::from_secret_bytes([tag ^ 0x55; 32]);
        let request = Request::create(
            [2; 16], [3; 32], [tag; 16], [tag; 16], &identity, &[tag; 32],
        )
        .unwrap();
        let record = Record::found(
            seed,
            vec![Entrant {
                request,
                node_key: [tag; 32],
                name: "cache-node".into(),
                label: format!("m{tag}"),
            }],
            1,
        )
        .unwrap();
        encode_records([&record])
    }

    fn base(seed: &[u8; 32]) -> Store {
        Store::dormant(&[envelope::signer_public(seed).unwrap()])
    }

    #[test]
    fn equal_fresh_membership_bytes_need_no_repeated_digest() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(home.path());
        fsx::ensure_private_dir(&paths.dir).unwrap();
        let wire = signed(&SIGNER, 1);
        fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
        let cache = Cache::new();
        for _ in 0..2 {
            assert_eq!(
                cache
                    .read(&paths, "fixture".into(), base(&SIGNER))
                    .unwrap()
                    .encode(),
                wire
            );
        }
        assert_eq!(cache.hashes.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn warmed_membership_still_reads_current_bytes_and_admitted_keys() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(home.path());
        fsx::ensure_private_dir(&paths.dir).unwrap();
        let first = signed(&SIGNER, 1);
        let second = signed(&OTHER, 2);
        fsx::write_atomic(&paths.records_file(), &first, 0o600).unwrap();
        let cache = Cache::new();
        assert!(
            cache
                .read(&paths, "fixture".into(), base(&SIGNER))
                .unwrap()
                .log()
                .is_some()
        );
        assert!(
            cache
                .read(&paths, "fixture".into(), base(&OTHER))
                .unwrap()
                .log()
                .is_none()
        );
        fsx::write_atomic(&paths.records_file(), &second, 0o600).unwrap();
        assert!(
            cache
                .read(&paths, "fixture".into(), base(&SIGNER))
                .unwrap()
                .log()
                .is_none()
        );
        assert_eq!(
            cache
                .read(&paths, "fixture".into(), base(&OTHER))
                .unwrap()
                .encode(),
            second
        );
        fsx::write_atomic(&paths.records_file(), b"malformed records", 0o600).unwrap();
        assert!(
            cache
                .read(&paths, "fixture".into(), base(&SIGNER))
                .unwrap()
                .log()
                .is_none()
        );
        fsx::write_atomic(&paths.records_file(), &first, 0o600).unwrap();
        assert_eq!(
            cache
                .read(&paths, "fixture".into(), base(&SIGNER))
                .unwrap()
                .encode(),
            first
        );
        std::fs::rename(paths.records_file(), paths.dir.join("held-records")).unwrap();
        assert!(
            cache
                .read(&paths, "fixture".into(), base(&SIGNER))
                .unwrap()
                .log()
                .is_none()
        );
    }

    #[test]
    fn large_membership_inputs_keep_the_existing_digest_path() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(home.path());
        fsx::ensure_private_dir(&paths.dir).unwrap();
        fsx::write_atomic(&paths.records_file(), &vec![0; MAX_INPUT_BYTES + 1], 0o600).unwrap();
        let cache = Cache::new();
        for _ in 0..2 {
            assert!(
                cache
                    .read(&paths, "fixture".into(), base(&SIGNER))
                    .unwrap()
                    .log()
                    .is_none()
            );
        }
        assert_eq!(cache.hashes.load(Ordering::Relaxed), 2);
    }
}
