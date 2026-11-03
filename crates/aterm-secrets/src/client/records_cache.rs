// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Recovered membership records under the current vault and admitted key set.

use super::{Paths, fsx, sha256};
use crate::{
    Error, SecretBytes,
    membership_log::{MAX_RECORDS_STREAM, Store},
};
use aterm_digest::Sha256;
use std::{
    collections::VecDeque,
    io::{Read, Seek, SeekFrom},
    sync::Mutex,
};
use zeroize::Zeroizing;

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
        self.read_then(paths, vault, store, || {})
    }

    fn read_then(
        &self,
        paths: &Paths,
        vault: String,
        store: Store,
        after_observation: impl FnOnce(),
    ) -> crate::Result<Store> {
        let Some(mut reader) = fsx::open_owned_reader(&paths.records_file())? else {
            return Ok(store);
        };
        let observed = observe(&mut reader)?;
        reader.check_unchanged()?;
        after_observation();
        let keys = store.keys().map(<[[u8; 32]]>::to_vec);
        let same_context = |entry: &Entry| entry.vault == vault && entry.keys == keys;
        // A recovered store roots another genesis under different admitted
        // keys. Reuse only after reading the current bytes and key context.
        if let Observed::Small(bytes) = &observed
            && let Ok(entries) = self.entries.lock()
            && let Some(entry) = entries.iter().find(|entry| {
                same_context(entry)
                    && entry
                        .bytes
                        .as_ref()
                        .is_some_and(|saved| saved.as_slice() == bytes.as_slice())
            })
        {
            reader.check_unchanged()?;
            return Ok(entry.store.clone());
        }
        #[cfg(test)]
        self.hashes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let digest = match &observed {
            Observed::Small(bytes) => sha256(bytes.as_slice()),
            Observed::Large(digest) => *digest,
        };
        let same = |entry: &Entry| same_context(entry) && entry.digest == digest;
        if let Ok(entries) = self.entries.lock()
            && let Some(entry) = entries.iter().find(|entry| same(entry))
        {
            reader.check_unchanged()?;
            return Ok(entry.store.clone());
        }
        let (store, bytes) = match observed {
            Observed::Small(bytes) => (store.recovering(bytes.as_slice()), Some(bytes)),
            Observed::Large(_) => {
                reader
                    .file()
                    .seek(SeekFrom::Start(0))
                    .map_err(|e| Error::new(format!("membership record seek failed: {e}")))?;
                let recovered = recover_observation(
                    store,
                    reader.file().take(MAX_RECORDS_STREAM as u64),
                    digest,
                )?;
                #[cfg(test)]
                self.hashes
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                (recovered, None)
            }
        };
        reader.check_unchanged()?;
        if let Ok(mut entries) = self.entries.lock() {
            entries.retain(|entry| !same(entry));
            if entries.len() >= MAX_ENTRIES {
                entries.pop_front();
            }
            entries.push_back(Entry {
                vault,
                keys,
                digest,
                bytes,
                store: store.clone(),
            });
        }
        Ok(store)
    }
}

enum Observed {
    Small(SecretBytes),
    Large([u8; 32]),
}

fn recover_observation(store: Store, input: impl Read, expected: [u8; 32]) -> crate::Result<Store> {
    let mut input = Hashed::new(input);
    let recovered = store.recovering_read(&mut input)?;
    if input.finish() != expected {
        return Err(Error::new("membership records changed between reads"));
    }
    Ok(recovered)
}

fn observe(reader: &mut fsx::OwnedReader) -> crate::Result<Observed> {
    let mut small = (reader.len() <= MAX_INPUT_BYTES as u64)
        .then(|| Zeroizing::new(Vec::with_capacity(reader.len() as usize + 1)));
    let mut digest = Sha256::new();
    let mut buffer = Zeroizing::new([0; 16 * 1024]);
    let mut input = reader.file().take(MAX_RECORDS_STREAM as u64);
    loop {
        let n = match input.read(&mut *buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::new(format!("membership record read failed: {e}"))),
        };
        if let Some(bytes) = &mut small {
            if bytes.len() + n <= MAX_INPUT_BYTES {
                bytes.extend_from_slice(&buffer[..n]);
                continue;
            }
            digest.update(bytes.as_slice());
            small = None;
        }
        digest.update(&buffer[..n]);
    }
    Ok(match small {
        Some(mut bytes) => Observed::Small(SecretBytes::new(std::mem::take(&mut *bytes))),
        None => Observed::Large(digest.finalize()),
    })
}

struct Hashed<R> {
    reader: R,
    digest: Sha256,
}

impl<R> Hashed<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            digest: Sha256::new(),
        }
    }

    fn finish(self) -> [u8; 32] {
        self.digest.finalize()
    }
}

impl<R: Read> Read for Hashed<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let n = self.reader.read(bytes)?;
        self.digest.update(&bytes[..n]);
        Ok(n)
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
        // First observation and its cold decode, then the second warm observation.
        assert_eq!(cache.hashes.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn changed_files_cannot_return_warm_small_or_large_records() {
        for large in [false, true] {
            let home = aterm_tempfile::tempdir().unwrap();
            let paths = Paths::for_home(home.path());
            fsx::ensure_private_dir(&paths.dir).unwrap();
            let mut wire = signed(&SIGNER, 1);
            if large {
                wire.resize(MAX_INPUT_BYTES + 64, 0);
            }
            fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
            let cache = Cache::new();
            let expected = cache
                .read(&paths, "fixture".into(), base(&SIGNER))
                .unwrap()
                .encode();
            let prior_digest = cache.entries.lock().unwrap()[0].digest;
            let changed = cache.read_then(&paths, "fixture".into(), base(&SIGNER), || {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(paths.records_file())
                    .unwrap()
                    .set_len(0)
                    .unwrap();
            });
            assert!(changed.is_err());
            {
                let entries = cache.entries.lock().unwrap();
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].digest, prior_digest);
            }
            fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
            assert_eq!(
                cache
                    .read(&paths, "fixture".into(), base(&SIGNER))
                    .unwrap()
                    .encode(),
                expected
            );
        }
    }

    #[test]
    fn changed_large_decode_never_installs_its_partial_store() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(home.path());
        fsx::ensure_private_dir(&paths.dir).unwrap();
        let mut wire = signed(&SIGNER, 1);
        wire.resize(MAX_INPUT_BYTES + 64, 0);
        fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
        let cache = Cache::new();
        let changed = cache.read_then(&paths, "fixture".into(), base(&SIGNER), || {
            std::fs::OpenOptions::new()
                .write(true)
                .open(paths.records_file())
                .unwrap()
                .set_len(0)
                .unwrap();
        });
        assert!(changed.is_err());
        assert!(cache.entries.lock().unwrap().is_empty());
        fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
        assert!(
            cache
                .read(&paths, "fixture".into(), base(&SIGNER))
                .unwrap()
                .log()
                .is_some()
        );
    }

    #[test]
    fn a_failed_observation_preserves_the_previous_cache_and_can_recover() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(home.path());
        fsx::ensure_private_dir(&paths.dir).unwrap();
        let wire = signed(&SIGNER, 1);
        fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
        let cache = Cache::new();
        cache.read(&paths, "fixture".into(), base(&SIGNER)).unwrap();
        let digest = cache.entries.lock().unwrap()[0].digest;
        fsx::fail_reads(&paths.records_file(), 1);
        assert!(cache.read(&paths, "fixture".into(), base(&SIGNER)).is_err());
        assert_eq!(cache.entries.lock().unwrap()[0].digest, digest);
        assert_eq!(
            cache
                .read(&paths, "fixture".into(), base(&SIGNER))
                .unwrap()
                .encode(),
            wire
        );
    }

    #[test]
    fn the_decoding_digest_includes_the_entire_ignored_tail() {
        let mut wire = signed(&SIGNER, 1);
        wire.resize(MAX_INPUT_BYTES + 64, 0);
        let mut reader = Hashed::new(std::io::Cursor::new(&wire));
        let recovered = base(&SIGNER).recovering_read(&mut reader).unwrap();
        assert!(recovered.log().is_some());
        assert_eq!(reader.finish(), sha256(&wire));
    }

    #[test]
    fn reversion_after_decoder_reads_cannot_authenticate_different_bytes() {
        use std::os::unix::fs::FileExt;

        struct Reverting<'a> {
            file: std::fs::File,
            original: &'a [u8],
            replacement: &'a [u8],
            read: usize,
            changed: bool,
            restored: bool,
        }
        impl Read for Reverting<'_> {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                if !self.changed {
                    self.file.write_all_at(self.replacement, 0)?;
                    self.changed = true;
                }
                let n = self.file.read(out)?;
                self.read += n;
                if self.read == self.original.len() && !self.restored {
                    self.file.write_all_at(self.original, 0)?;
                    self.restored = true;
                }
                Ok(n)
            }
        }

        let home = aterm_tempfile::tempdir().unwrap();
        let path = home.path().join("records");
        let mut original = signed(&SIGNER, 1);
        let mut replacement = signed(&SIGNER, 2);
        original.resize(MAX_INPUT_BYTES + 64, 0);
        replacement.resize(original.len(), 0);
        assert!(base(&SIGNER).recovering(&replacement).log().is_some());
        fsx::write_atomic(&path, &original, 0o600).unwrap();
        let mut reader = Reverting {
            file: std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap(),
            original: &original,
            replacement: &replacement,
            read: 0,
            changed: false,
            restored: false,
        };
        let outcome = recover_observation(base(&SIGNER), &mut reader, sha256(&original));
        assert!(reader.changed && reader.restored);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(outcome.is_err());
    }
}
