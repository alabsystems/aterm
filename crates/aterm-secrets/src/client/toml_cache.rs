// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Immutable parse trees for exact, freshly read client TOML bytes. File admission
//! and size checks run before every lookup. This stores no interpreted source,
//! signer lineage, applied-state decision or authorization.

use crate::{Error, SecretBytes, fsx};
use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
};

const MAX_ENTRIES: usize = 8;
const MAX_INPUT_BYTES: usize = 16 * 1024;
const MAX_TOTAL_BYTES: usize = 64 * 1024;

pub(super) fn read(
    path: &Path,
    what: &str,
    max: usize,
) -> crate::Result<Option<Arc<aterm_toml::Table>>> {
    static CACHE: Cache = Cache::new();
    CACHE.read(path, what, max)
}

struct Entry {
    bytes: SecretBytes,
    table: Arc<aterm_toml::Table>,
}

struct Cache {
    entries: Mutex<VecDeque<Entry>>,
    #[cfg(test)]
    parses: std::sync::atomic::AtomicUsize,
}

#[derive(Debug, PartialEq, Eq)]
enum ParseError {
    Utf8,
    Toml,
}

fn parse_uncached(bytes: &[u8]) -> Result<aterm_toml::Table, ParseError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ParseError::Utf8)?;
    match aterm_toml::from_str::<aterm_toml::Value>(text) {
        Ok(aterm_toml::Value::Table(table)) => Ok(table),
        _ => Err(ParseError::Toml),
    }
}

impl Cache {
    const fn new() -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            #[cfg(test)]
            parses: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn read(
        &self,
        path: &Path,
        what: &str,
        max: usize,
    ) -> crate::Result<Option<Arc<aterm_toml::Table>>> {
        // A warm parse never substitutes for opening and admitting this file now.
        let Some(bytes) = fsx::read_private(path, max)? else {
            return Ok(None);
        };
        self.parse(bytes).map(Some).map_err(|error| match error {
            ParseError::Utf8 => Error::new(format!("{what}: not UTF-8")),
            ParseError::Toml => Error::new(format!(
                "{}: not a valid {what}; move it aside and run aterm secrets join again",
                path.display()
            )),
        })
    }

    fn parse(&self, bytes: SecretBytes) -> Result<Arc<aterm_toml::Table>, ParseError> {
        let keep = bytes.len() <= MAX_INPUT_BYTES;
        if keep
            && let Ok(entries) = self.entries.lock()
            && let Some(entry) = entries.iter().find(|entry| entry.bytes == bytes)
        {
            return Ok(Arc::clone(&entry.table));
        }

        // Parsing holds no cache lock. A poisoned or unavailable cache still
        // permits the same fresh parse; errors are never retained.
        #[cfg(test)]
        self.parses
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let table = Arc::new(parse_uncached(bytes.as_slice())?);
        if keep && let Ok(mut entries) = self.entries.lock() {
            // A concurrent parser may have published these exact bytes already.
            if let Some(entry) = entries.iter().find(|entry| entry.bytes == bytes) {
                return Ok(Arc::clone(&entry.table));
            }
            while entries.len() >= MAX_ENTRIES
                || entries
                    .iter()
                    .map(|entry| entry.bytes.len())
                    .sum::<usize>()
                    .saturating_add(bytes.len())
                    > MAX_TOTAL_BYTES
            {
                entries.pop_front();
            }
            entries.push_back(Entry {
                bytes,
                table: Arc::clone(&table),
            });
        }
        Ok(table)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{client, envelope};
    use std::{os::unix::fs::PermissionsExt, sync::atomic::Ordering};

    fn value(table: &aterm_toml::Table) -> i64 {
        table.get("value").unwrap().as_integer().unwrap()
    }

    #[test]
    fn identical_bytes_share_a_tree_and_edits_are_parsed_afresh() {
        let cache = Cache::new();
        let first = cache
            .parse(SecretBytes::from("value = 1\n".to_owned()))
            .unwrap();
        let same = cache
            .parse(SecretBytes::from("value = 1\n".to_owned()))
            .unwrap();
        assert!(Arc::ptr_eq(&first, &same));
        assert_eq!(cache.parses.load(Ordering::Relaxed), 1);
        let next = cache
            .parse(SecretBytes::from("value = 2\n".to_owned()))
            .unwrap();
        assert_eq!(value(&next), 2);
        assert_eq!(value(&first), 1, "a held observation remains immutable");
        assert_eq!(cache.parses.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn a_warm_reader_still_checks_the_file_bytes_permissions_and_size() {
        let home = aterm_tempfile::tempdir().unwrap();
        let path = home.path().join("source.toml");
        let cache = Cache::new();
        let read = |max| cache.read(&path, "source.toml", max);
        fsx::write_atomic(&path, b"value = 1\n", 0o600).unwrap();
        assert_eq!(value(&read(100).unwrap().unwrap()), 1);
        fsx::write_atomic(&path, b"value = 2\n", 0o600).unwrap();
        assert_eq!(value(&read(100).unwrap().unwrap()), 2);
        assert!(
            read(4).is_err(),
            "cached bytes do not bypass a smaller bound"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            read(100).is_err(),
            "a warm file is still admitted on each read"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        fsx::write_atomic(&path, b"value = [", 0o600).unwrap();
        assert!(read(100).is_err());
        fsx::write_atomic(&path, &[0xff], 0o600).unwrap();
        assert!(read(100).is_err(), "invalid UTF-8 remains refused");
        fsx::write_atomic(&path, b"value = 3\n", 0o600).unwrap();
        assert_eq!(value(&read(100).unwrap().unwrap()), 3);
        let held = home.path().join("held.toml");
        std::fs::rename(&path, &held).unwrap();
        assert!(read(100).unwrap().is_none());
        std::os::unix::fs::symlink(&held, &path).unwrap();
        assert!(read(100).is_err(), "a symlink cannot borrow the old parse");
    }

    fn signed(previous: &[u8; 32]) -> Vec<u8> {
        const SIGNER: [u8; 32] = [61; 32];
        let signer = envelope::signer_public(&SIGNER).unwrap();
        envelope::sign(
            &envelope::Unsigned {
                name: "toml-cache".into(),
                seq: 11,
                time: 1_792_530_000,
                handoff: vec![(
                    envelope::signer_public(previous).unwrap(),
                    envelope::endorse(previous, "toml-cache", &signer).unwrap(),
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
    fn a_shared_source_parse_never_caches_a_homes_signer_lineage() {
        let home = aterm_tempfile::tempdir().unwrap();
        let a = client::Paths::for_home(&home.path().join("a"));
        let b = client::Paths::for_home(&home.path().join("b"));
        let signer = envelope::signer_public(&[61; 32]).unwrap();
        let source = format!(
            "url = \"http://127.0.0.1:9\"\nname = \"toml-cache\"\nsigner = \"{}\"\n",
            crate::b64::encode_padded(&signer)
        );
        for paths in [&a, &b] {
            fsx::ensure_private_dir(&paths.dir).unwrap();
            fsx::write_atomic(&paths.source_file(), source.as_bytes(), 0o600).unwrap();
        }
        fsx::write_atomic(&a.cache_file(), &signed(&[33; 32]), 0o600).unwrap();
        fsx::write_atomic(&b.cache_file(), &signed(&[47; 32]), 0o600).unwrap();
        let first = client::config(&a).unwrap().unwrap();
        let second = client::config(&b).unwrap().unwrap();
        assert_eq!(
            first.followed,
            [envelope::signer_public(&[33; 32]).unwrap()]
        );
        assert_eq!(
            second.followed,
            [envelope::signer_public(&[47; 32]).unwrap()]
        );
        fsx::write_atomic(&a.cache_file(), &signed(&[47; 32]), 0o600).unwrap();
        assert_eq!(
            client::config(&a).unwrap().unwrap().followed,
            second.followed
        );
    }

    #[test]
    fn entry_and_byte_bounds_leave_large_valid_inputs_uncached() {
        let cache = Cache::new();
        let first = cache
            .parse(SecretBytes::from("value = 0\n".to_owned()))
            .unwrap();
        for n in 1..=MAX_ENTRIES + 2 {
            let text = format!("value = {n}\n#{}", " ".repeat(MAX_INPUT_BYTES - 32));
            assert_eq!(
                value(&cache.parse(SecretBytes::from(text)).unwrap()),
                n as i64
            );
            let entries = cache.entries.lock().unwrap();
            assert!(entries.len() <= MAX_ENTRIES);
            assert!(
                entries.iter().map(|entry| entry.bytes.len()).sum::<usize>() <= MAX_TOTAL_BYTES
            );
        }
        assert_eq!(value(&first), 0);
        let large = format!("value = 99\n#{}", " ".repeat(MAX_INPUT_BYTES));
        let before = cache.parses.load(Ordering::Relaxed);
        for _ in 0..2 {
            assert_eq!(
                value(&cache.parse(SecretBytes::from(large.clone())).unwrap()),
                99
            );
        }
        assert_eq!(cache.parses.load(Ordering::Relaxed) - before, 2);
    }

    #[test]
    fn unchanged_client_toml_parse_cost() {
        use std::{hint::black_box, time::Instant};
        const SAMPLES: u32 = 1024;
        let source = "url = \"http://127.0.0.1:9\"\nname = \"fixture\"\nhost = \"m1\"\n".to_owned();
        let state = format!(
            "membership = \"in\"\nseq = 42\n[manifest]\n{}",
            (0..32)
                .map(|n| format!("\"config/{n}\" = \"{}\"\n", "a".repeat(64)))
                .collect::<String>()
        );
        for (label, text) in [("source", source), ("state", state)] {
            let make = || SecretBytes::from(text.clone());
            let started = Instant::now();
            for _ in 0..SAMPLES {
                black_box(parse_uncached(make().as_slice()).unwrap());
            }
            let uncached = started.elapsed();
            let cache = Cache::new();
            let expected = cache.parse(make()).unwrap();
            let started = Instant::now();
            for _ in 0..SAMPLES {
                black_box(cache.parse(make()).unwrap());
            }
            let cached = started.elapsed();
            assert_eq!(*expected, parse_uncached(text.as_bytes()).unwrap());
            assert_eq!(cache.parses.load(Ordering::Relaxed), 1);
            eprintln!(
                "CLIENT-TOML-PARSE input={label} bytes={} n={SAMPLES} uncached_per={:?} cached_per={:?}",
                text.len(),
                uncached / SAMPLES,
                cached / SAMPLES,
            );
        }
    }
}
