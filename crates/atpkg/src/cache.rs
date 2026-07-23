// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Same-source index cache (§14) — persist the last successfully-fetched index CANDIDATE
//! bytes so a transient fetch failure falls back to the last-good SIGNED index.
//!
//! The cache holds **bytes, not trust**: cached candidates are fed back UNCHANGED through
//! [`crate::select::select_index`]'s verify-then-select plus the caller's floor + freshness
//! gates, so a tampered/stale cache installs nothing the live path wouldn't. It is honored
//! ONLY when its recorded source == the current fetcher's `source_id`, so a `dir:`
//! publisher-test cache never satisfies a failed `github:` fetch (the same-source guard).
//!
//! Parsing the cache TOML is NOT the verify-before-parse boundary: the extracted
//! `index_bytes` are only ever handed to `select_index` → `verify_index_with` (sig checked
//! over exact bytes under the pinned root) → `parse_index`; a forged cache fails that verify.

use std::fs;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

use crate::select::Candidate;

/// One cached index candidate: its label + base64 index/sig bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedCandidate {
    label: String,
    index_b64: String,
    sig_b64: String,
}

/// The on-disk cache document.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheDoc {
    schema: u32,
    /// The fetcher `source_id` these candidates came from (the same-source guard key).
    source: String,
    #[serde(default)]
    fetched_at: String,
    #[serde(default)]
    candidate: Vec<CachedCandidate>,
}

/// A file-backed index cache under the hardened prefix.
pub struct IndexCache {
    path: PathBuf,
}

impl IndexCache {
    /// A cache backed by `path` (typically `<prefix>/index-cache.toml`).
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Persist `candidates` tagged with `source_id`. Never clobbers a good cache with an
    /// empty "success" (an empty candidate set is not stored). Best-effort — any error is
    /// swallowed so a cache-write failure never fails an install. Written `0600` via
    /// temp + rename so a reader never sees a half-written cache.
    pub fn store(&self, source_id: &str, candidates: &[Candidate]) {
        if candidates.is_empty() {
            return; // never overwrite a good cache with an empty success
        }
        let doc = CacheDoc {
            schema: 1,
            source: source_id.to_string(),
            fetched_at: String::new(),
            candidate: candidates
                .iter()
                .map(|c| CachedCandidate {
                    label: c.label.clone(),
                    index_b64: STANDARD.encode(&c.index_bytes),
                    sig_b64: STANDARD.encode(&c.sig),
                })
                .collect(),
        };
        let Ok(text) = toml::to_string(&doc) else {
            return;
        };
        let Some(parent) = self.path.parent() else {
            return;
        };
        // The cache is written at the very START of an install, before the flow creates any
        // prefix dir — harden/create the (vetted) parent so the first fetch is cached too.
        if crate::platform::ensure_private_dir(parent).is_err() {
            return;
        }
        let tmp = parent.join(format!(".index-cache.tmp-{}", std::process::id()));
        if fs::write(&tmp, text.as_bytes()).is_err() {
            return;
        }
        let _ = crate::platform::harden_file(&tmp);
        let _ = fs::rename(&tmp, &self.path);
    }

    /// Load the cached candidates IFF the cache's recorded source == `source_id` (the
    /// same-source guard). Fail-closed to `None` on any missing/parse/decode error, so a
    /// garbage cache is inert rather than trusted.
    #[must_use]
    pub fn load(&self, source_id: &str) -> Option<Vec<Candidate>> {
        let text = fs::read_to_string(&self.path).ok()?;
        let doc: CacheDoc = toml::from_str(&text).ok()?;
        if doc.source != source_id {
            return None; // SAME-SOURCE GUARD: a dir: cache never satisfies a github: fetch
        }
        let mut out = Vec::with_capacity(doc.candidate.len());
        for c in doc.candidate {
            let index_bytes = STANDARD.decode(c.index_b64.as_bytes()).ok()?;
            let sig = STANDARD.decode(c.sig_b64.as_bytes()).ok()?;
            out.push(Candidate {
                label: c.label,
                index_bytes,
                sig,
            });
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn tmp(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("atpkg-cache-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d.join("index-cache.toml")
    }

    fn cand(label: &str) -> Candidate {
        Candidate {
            label: label.into(),
            index_bytes: vec![1, 2, 3, 0xFF],
            sig: vec![9, 8, 7],
        }
    }

    #[test]
    fn store_then_load_same_source_round_trips() {
        let p = tmp("rt");
        let c = IndexCache::new(p.clone());
        c.store("github:o/r", &[cand("v1"), cand("v0")]);
        let back = c.load("github:o/r").expect("same source loads");
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].label, "v1");
        assert_eq!(back[0].index_bytes, vec![1, 2, 3, 0xFF]);
        assert_eq!(back[0].sig, vec![9, 8, 7]);
        // 0600 on disk — Unix-only mode check.
        #[cfg(unix)]
        {
            let mode = fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn load_rejects_other_source() {
        let p = tmp("other");
        let c = IndexCache::new(p.clone());
        c.store("dir:/tmp/reg", &[cand("v1")]);
        assert!(
            c.load("github:o/r").is_none(),
            "same-source guard blocks cross-source reuse"
        );
        assert!(c.load("dir:/tmp/reg").is_some());
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn load_none_on_missing_or_garbage() {
        let p = tmp("garbage");
        let c = IndexCache::new(p.clone());
        assert!(c.load("any").is_none(), "absent cache is None");
        fs::write(&p, "this is not valid toml {{{").unwrap();
        assert!(c.load("any").is_none(), "garbage cache is fail-closed None");
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn store_skips_empty_candidate_set() {
        let p = tmp("empty");
        let c = IndexCache::new(p.clone());
        c.store("github:o/r", &[cand("v1")]); // good cache
        c.store("github:o/r", &[]); // an empty "success" must NOT clobber it
        assert_eq!(
            c.load("github:o/r").unwrap().len(),
            1,
            "good cache preserved"
        );
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }
}
