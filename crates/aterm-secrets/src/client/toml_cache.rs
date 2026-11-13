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
#[path = "toml_cache_tests.rs"]
mod tests;
