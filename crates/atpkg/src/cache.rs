// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Same-source index cache for fallback on transient fetch failure.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::select::Candidate;
use crate::store::Layout;

const CACHE_SCHEMA: u32 = 1;
const MAX_CACHE_CANDIDATES: usize = 24;
const MAX_CACHED_INDEX_BYTES: usize = 5_000_000;
const MAX_CACHED_SIGNATURE_BYTES: usize = 4_096;
const MAX_CACHED_ROSTER_BYTES: usize = 65_536;
const MAX_CACHED_LABEL_BYTES: usize = 512;
const MAX_INDEX_CACHE_BYTES: usize = 144 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedCandidate {
    label: String,
    index_b64: String,
    sig_b64: String,
    #[serde(default)]
    roster_b64: String,
    #[serde(default)]
    roster_sig_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheDoc {
    schema: u32,
    source: String,
    #[serde(default)]
    fetched_at: String,
    #[serde(default)]
    candidate: Vec<CachedCandidate>,
}

/// A file-backed index cache under the hardened prefix.
pub struct IndexCache {
    path: PathBuf,
    layout: Option<Layout>,
}

impl IndexCache {
    /// A cache backed by `path` with no prefix shape to follow: `store` makes the parent `0700`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path, layout: None }
    }

    /// The `<prefix>/index-cache.toml` cache of `layout`, the constructor every install path uses.
    #[must_use]
    pub fn for_layout(layout: &Layout) -> Self {
        Self {
            path: layout.prefix.join("index-cache.toml"),
            layout: Some(layout.clone()),
        }
    }

    /// Persist `candidates` tagged with `source_id`.
    pub fn store(&self, source_id: &str, candidates: &[Candidate]) {
        if candidates.is_empty()
            || candidates.len() > MAX_CACHE_CANDIDATES
            || candidates.iter().any(|candidate| {
                candidate.label.len() > MAX_CACHED_LABEL_BYTES
                    || candidate.index_bytes.len() > MAX_CACHED_INDEX_BYTES
                    || candidate.sig.len() > MAX_CACHED_SIGNATURE_BYTES
                    || candidate.roster_bytes.len() > MAX_CACHED_ROSTER_BYTES
                    || candidate.roster_sig.len() > MAX_CACHED_SIGNATURE_BYTES
            })
        {
            return; // never overwrite a good cache with an empty success
        }
        let doc = CacheDoc {
            schema: CACHE_SCHEMA,
            source: source_id.to_string(),
            fetched_at: String::new(),
            candidate: {
                let mut rows = Vec::with_capacity(candidates.len());
                for c in candidates {
                    let (Ok(index_b64), Ok(sig_b64), Ok(roster_b64), Ok(roster_sig_b64)) = (
                        aterm_codec::base64::encode(&c.index_bytes),
                        aterm_codec::base64::encode(&c.sig),
                        aterm_codec::base64::encode(&c.roster_bytes),
                        aterm_codec::base64::encode(&c.roster_sig),
                    ) else {
                        return;
                    };
                    rows.push(CachedCandidate {
                        label: c.label.clone(),
                        index_b64,
                        sig_b64,
                        roster_b64,
                        roster_sig_b64,
                    });
                }
                rows
            },
        };
        let Ok(text) = aterm_toml::to_string(&doc) else {
            return;
        };
        if text.len() > MAX_INDEX_CACHE_BYTES {
            return;
        }
        let Some(parent) = self.path.parent() else {
            return;
        };
        if self.ensure_parent(parent).is_err() {
            return;
        }
        if crate::metadata_io::read_bounded_regular_utf8(&self.path, MAX_INDEX_CACHE_BYTES)
            .is_ok_and(|on_disk| on_disk == text)
        {
            let _ = crate::platform::harden_file(&self.path);
            return;
        }
        let _ = crate::metadata_io::write_private_atomic(&self.path, text.as_bytes());
    }

    fn ensure_parent(&self, parent: &Path) -> std::io::Result<()> {
        match &self.layout {
            Some(layout) => layout.ensure_dir(parent),
            None => crate::platform::ensure_private_dir(parent),
        }
    }

    /// Load the cached candidates iff the cache's recorded source is `source_id` (the same-source
    /// guard).
    #[must_use]
    pub fn load(&self, source_id: &str) -> Option<Vec<Candidate>> {
        decode(self.read_doc(source_id)?)
    }

    /// The release labels of the candidates cached for `source_id`, newest first, under the same
    /// guards as [`Self::load`] and without decoding them.
    #[must_use]
    pub fn labels(&self, source_id: &str) -> Option<Vec<String>> {
        Some(
            self.read_doc(source_id)?
                .candidate
                .into_iter()
                .map(|c| c.label)
                .collect(),
        )
    }

    fn read_doc(&self, source_id: &str) -> Option<CacheDoc> {
        let text = crate::metadata_io::read_bounded_regular_utf8(&self.path, MAX_INDEX_CACHE_BYTES)
            .ok()?;
        let doc: CacheDoc = aterm_toml::from_str(&text).ok()?;
        if doc.schema != CACHE_SCHEMA {
            return None;
        }
        if doc.source != source_id || doc.candidate.len() > MAX_CACHE_CANDIDATES {
            return None; // the same-source guard
        }
        if doc
            .candidate
            .iter()
            .any(|c| c.label.len() > MAX_CACHED_LABEL_BYTES)
        {
            return None;
        }
        Some(doc)
    }
}

fn decode(doc: CacheDoc) -> Option<Vec<Candidate>> {
    let mut out = Vec::with_capacity(doc.candidate.len());
    for c in doc.candidate {
        let index_bytes = aterm_codec::base64::decode_strict(c.index_b64.as_bytes()).ok()?;
        let sig = aterm_codec::base64::decode_strict(c.sig_b64.as_bytes()).ok()?;
        let roster_bytes = aterm_codec::base64::decode_strict(c.roster_b64.as_bytes()).ok()?;
        let roster_sig = aterm_codec::base64::decode_strict(c.roster_sig_b64.as_bytes()).ok()?;
        if index_bytes.len() > MAX_CACHED_INDEX_BYTES
            || sig.len() > MAX_CACHED_SIGNATURE_BYTES
            || roster_bytes.len() > MAX_CACHED_ROSTER_BYTES
            || roster_sig.len() > MAX_CACHED_SIGNATURE_BYTES
        {
            return None;
        }
        out.push(Candidate {
            label: c.label,
            index_bytes,
            sig,
            roster_bytes,
            roster_sig,
        });
    }
    Some(out)
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
