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
//! Parsing the cache TOML is NOT the verify-before-parse boundary: the extracted bytes are
//! only ever handed to [`crate::select_index`], which admits the cached ROSTER under the
//! pinned paper master and then verifies the cached index under the machines that roster
//! still authorizes, before `parse_index` sees a byte. A forged cache fails that chain.
//!
//! # The roster is cached WITH its index, and that is what keeps the cache honest
//!
//! A cache entry stores the whole candidate — index, index signature, roster, roster
//! signature — because the index is meaningless without the generation that authorized it.
//! Storing only the index would have made the cache a downgrade oracle: whoever serves the
//! index could suppress the roster assets, and a client that fell back to a cached index
//! plus a freshly-fetched roster would be pairing documents that were never published
//! together.
//!
//! The cached roster is not trusted for being cached. It faces the same `roster_seq`
//! ratchet (a generation older than the durable floor is refused forever) and the same
//! `valid_until` window (a cache that outlives the roster's freshness stops working, by
//! design — that window IS the bound on how long a suppressed roster can keep an old
//! authorization alive). An entry written before this field existed decodes to EMPTY
//! roster bytes, which fail admission — fail-closed, and the next successful fetch
//! replaces it.

use std::fs;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

use crate::select::Candidate;

const MAX_CACHE_CANDIDATES: usize = 24;
const MAX_CACHED_INDEX_BYTES: usize = 5_000_000;
const MAX_CACHED_SIGNATURE_BYTES: usize = 4_096;
/// A roster is a few hundred bytes per machine and is capped at 16 machines; 64 KiB is a
/// ceiling, not a fit. Matches the download cap `net` and `aterm-update` both use.
const MAX_CACHED_ROSTER_BYTES: usize = 65_536;
const MAX_INDEX_CACHE_BYTES: usize = 144 * 1024 * 1024;

/// One cached index candidate: its label + base64 index/sig bytes + the master-signed
/// roster published beside it.
///
/// The two roster fields are `#[serde(default)]` so a cache written by an older build
/// still DECODES — and then yields empty roster bytes, which no chain admits. That is the
/// correct direction: an entry that predates the single root cannot authorize anything,
/// and it is replaced by the next successful fetch rather than failing the read.
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
        if candidates.is_empty()
            || candidates.len() > MAX_CACHE_CANDIDATES
            || candidates.iter().any(|candidate| {
                candidate.index_bytes.len() > MAX_CACHED_INDEX_BYTES
                    || candidate.sig.len() > MAX_CACHED_SIGNATURE_BYTES
                    || candidate.roster_bytes.len() > MAX_CACHED_ROSTER_BYTES
                    || candidate.roster_sig.len() > MAX_CACHED_SIGNATURE_BYTES
            })
        {
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
                    roster_b64: STANDARD.encode(&c.roster_bytes),
                    roster_sig_b64: STANDARD.encode(&c.roster_sig),
                })
                .collect(),
        };
        let Ok(text) = toml::to_string(&doc) else {
            return;
        };
        if text.len() > MAX_INDEX_CACHE_BYTES {
            return;
        }
        let Some(parent) = self.path.parent() else {
            return;
        };
        // The cache is written at the very START of an install, before the flow creates any
        // prefix dir — harden/create the (vetted) parent so the first fetch is cached too.
        if crate::platform::ensure_private_dir(parent).is_err() {
            return;
        }
        // Already exactly what is on disk ⇒ nothing left to write. The install flow
        // resolves the candidates once per program AND once per transitive dependency,
        // and every successful resolve lands here with a byte-identical document, so this
        // collapses N temp-file writes + renames to one. Placed AFTER the parent
        // hardening (which must run either way) and guarded on the SAME bounded no-follow
        // reader `load` uses, so a symlinked/oversize/non-regular cache path never matches
        // and still takes the replacing temp+rename path below. The file mode is
        // re-asserted — a plain chmod on a path just read as a regular file — so the 0600
        // invariant a write establishes is not skipped along with the write.
        if crate::metadata_io::read_bounded_regular_utf8(&self.path, MAX_INDEX_CACHE_BYTES)
            .is_ok_and(|on_disk| on_disk == text)
        {
            let _ = crate::platform::harden_file(&self.path);
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
        let text = crate::metadata_io::read_bounded_regular_utf8(&self.path, MAX_INDEX_CACHE_BYTES)
            .ok()?;
        let doc: CacheDoc = toml::from_str(&text).ok()?;
        if doc.source != source_id || doc.candidate.len() > MAX_CACHE_CANDIDATES {
            return None; // SAME-SOURCE GUARD: a dir: cache never satisfies a github: fetch
        }
        let mut out = Vec::with_capacity(doc.candidate.len());
        for c in doc.candidate {
            let index_bytes = STANDARD.decode(c.index_b64.as_bytes()).ok()?;
            let sig = STANDARD.decode(c.sig_b64.as_bytes()).ok()?;
            let roster_bytes = STANDARD.decode(c.roster_b64.as_bytes()).ok()?;
            let roster_sig = STANDARD.decode(c.roster_sig_b64.as_bytes()).ok()?;
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
            roster_bytes: vec![0xDE, 0xAD, 0xBE, 0xEF],
            roster_sig: vec![4, 5, 6],
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
        // The roster rides WITH its index — an entry that lost it could only be paired
        // with some other generation, which is exactly the substitution to prevent.
        assert_eq!(back[0].roster_bytes, vec![0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(back[0].roster_sig, vec![4, 5, 6]);
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

    /// A cache written BEFORE the single-root move (no roster fields) still decodes —
    /// and decodes to EMPTY roster bytes, which no chain admits. Fail-closed, and
    /// replaced by the next successful fetch rather than failing the whole read.
    #[test]
    fn a_pre_roster_cache_entry_decodes_but_authorizes_nothing() {
        let p = tmp("legacy");
        fs::write(
            &p,
            "schema = 1\nsource = \"github:o/r\"\nfetched_at = \"\"\n\
             [[candidate]]\nlabel = \"v1\"\nindex_b64 = \"AQID\"\nsig_b64 = \"CQgH\"\n",
        )
        .unwrap();
        let back = IndexCache::new(p.clone())
            .load("github:o/r")
            .expect("an older entry is readable, not a hard failure");
        assert_eq!(back.len(), 1);
        assert!(
            back[0].roster_bytes.is_empty() && back[0].roster_sig.is_empty(),
            "a pre-roster entry carries no authority at all"
        );
        // And empty roster bytes cannot admit: `select_index` skips such a candidate
        // (proved directly in `select::tests::a_candidate_with_no_roster_is_skipped`).
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

    #[test]
    fn sparse_oversized_and_overcount_cache_fail_closed() {
        let p = tmp("bounded");
        let file = fs::File::create(&p).unwrap();
        file.set_len((MAX_INDEX_CACHE_BYTES + 1) as u64).unwrap();
        assert!(IndexCache::new(p.clone()).load("any").is_none());

        let candidates = vec![cand("x"); MAX_CACHE_CANDIDATES + 1];
        let cache = IndexCache::new(p.clone());
        fs::write(&p, "sentinel").unwrap();
        cache.store("any", &candidates);
        assert_eq!(fs::read_to_string(&p).unwrap(), "sentinel");
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn fifo_and_symlink_cache_return_without_blocking() {
        use std::os::unix::ffi::OsStrExt as _;

        let p = tmp("special");
        let p_c = std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
        // SAFETY: `p_c` is a live NUL-terminated path in our private fixture.
        assert_eq!(unsafe { libc::mkfifo(p_c.as_ptr(), 0o600) }, 0);
        assert!(IndexCache::new(p.clone()).load("any").is_none());
        fs::remove_file(&p).unwrap();
        let target = p.with_file_name("cache-target");
        IndexCache::new(target.clone()).store("any", &[cand("one")]);
        std::os::unix::fs::symlink(&target, &p).unwrap();
        assert!(IndexCache::new(p.clone()).load("any").is_none());
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }
}
