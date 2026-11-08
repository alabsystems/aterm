// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Bounded storage of untrusted signed pairs; the caller must authenticate each hit.

use serde::{Deserialize, Serialize};

use crate::store::Layout;

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_ENTRIES: usize = 16;
const MAX_LABEL: usize = 1024;

#[derive(Serialize, Deserialize)]
struct Entry {
    source: String,
    program: String,
    build: u64,
    raw: String,
    signature: String,
}

#[derive(Serialize, Deserialize)]
struct Document {
    schema: u32,
    entries: Vec<Entry>,
}

pub(super) struct Cache<'a> {
    layout: &'a Layout,
}

impl<'a> Cache<'a> {
    pub(super) fn new(layout: &'a Layout) -> Self {
        Self { layout }
    }

    fn path(&self) -> std::path::PathBuf {
        self.layout.prefix.join("prerequisite-manifests.toml")
    }

    fn read(&self) -> Option<Document> {
        let text = crate::metadata_io::read_bounded_regular_utf8(&self.path(), MAX_BYTES).ok()?;
        let doc: Document = aterm_toml::from_str(&text).ok()?;
        (doc.schema == 1
            && doc.entries.len() <= MAX_ENTRIES
            && doc
                .entries
                .iter()
                .all(|e| e.source.len() <= MAX_LABEL && e.program.len() <= MAX_LABEL))
        .then_some(doc)
    }

    pub(super) fn load(
        &self,
        source: &str,
        program: &str,
        build: u64,
    ) -> Option<(Vec<u8>, Vec<u8>)> {
        let entry = self
            .read()?
            .entries
            .into_iter()
            .find(|e| e.source == source && e.program == program && e.build == build)?;
        let raw = aterm_codec::base64::decode(&entry.raw).ok()?;
        let signature = aterm_codec::base64::decode(&entry.signature).ok()?;
        (raw.len() as u64 <= crate::net::MANIFEST_CAP
            && signature.len() as u64 <= crate::net::SIG_CAP)
            .then_some((raw, signature))
    }

    pub(super) fn store(&self, source: &str, program: &str, build: u64, pair: &(Vec<u8>, Vec<u8>)) {
        if source.len() > MAX_LABEL
            || program.len() > MAX_LABEL
            || pair.0.len() as u64 > crate::net::MANIFEST_CAP
            || pair.1.len() as u64 > crate::net::SIG_CAP
        {
            return;
        }
        let (Ok(raw), Ok(signature)) = (
            aterm_codec::base64::encode(&pair.0),
            aterm_codec::base64::encode(&pair.1),
        ) else {
            return;
        };
        let mut doc = self.read().unwrap_or(Document {
            schema: 1,
            entries: Vec::new(),
        });
        doc.entries
            .retain(|e| !(e.source == source && e.program == program && e.build == build));
        doc.entries.push(Entry {
            source: source.into(),
            program: program.into(),
            build,
            raw,
            signature,
        });
        while doc.entries.len() > MAX_ENTRIES {
            doc.entries.remove(0);
        }
        let text = loop {
            let Ok(text) = aterm_toml::to_string(&doc) else {
                return;
            };
            if text.len() <= MAX_BYTES {
                break text;
            }
            if doc.entries.len() <= 1 {
                return;
            }
            doc.entries.remove(0);
        };
        if self.layout.ensure_dir(&self.layout.prefix).is_err() {
            return;
        }
        // Lost concurrent updates cause a later miss, never trusted stale authority.
        let _ = crate::metadata_io::write_private_atomic(&self.path(), text.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_is_bounded_and_replaces_only_the_same_source_program_build() {
        let layout = crate::testdir::layout("manifest-cache-");
        let cache = Cache::new(&layout);
        let pair = (b"manifest".to_vec(), b"signature".to_vec());
        for build in 0..20 {
            cache.store("source", "program", build, &pair);
        }
        assert!(cache.load("source", "program", 3).is_none());
        assert_eq!(cache.load("source", "program", 4), Some(pair.clone()));
        assert!(cache.load("different", "program", 19).is_none());
        assert!(cache.load("source", "different", 19).is_none());
        cache.store("source", "program", 19, &(vec![1], vec![2]));
        assert_eq!(
            cache.load("source", "program", 19),
            Some((vec![1], vec![2]))
        );
        assert_eq!(cache.read().unwrap().entries.len(), MAX_ENTRIES);
        assert!(std::fs::metadata(cache.path()).unwrap().len() <= MAX_BYTES as u64);
    }

    #[test]
    #[cfg(unix)]
    fn symlinks_and_oversized_files_are_misses_and_writes_do_not_follow_them() {
        let layout = crate::testdir::layout("manifest-cache-");
        let cache = Cache::new(&layout);
        layout.ensure_dir(&layout.prefix).unwrap();
        let outside = layout.prefix.join("unrelated");
        std::fs::write(&outside, b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, cache.path()).unwrap();
        assert!(cache.load("source", "program", 1).is_none());
        let pair = (vec![1], vec![2]);
        cache.store("source", "program", 1, &pair);
        assert_eq!(std::fs::read(outside).unwrap(), b"keep");
        assert_eq!(cache.load("source", "program", 1), Some(pair));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(cache.path())
            .unwrap();
        file.set_len(MAX_BYTES as u64 + 1).unwrap();
        assert!(cache.load("source", "program", 1).is_none());
    }
}
