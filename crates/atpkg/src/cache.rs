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
//!
//! # The IDENTITY beside each entry is what turns this from a fallback into a hit path
//!
//! Each entry additionally records the `identity` of the release assets its bytes were
//! downloaded from — an opaque, fixed-width fingerprint the FETCHER computes
//! ([`crate::flow::Fetcher::index_identities`]). It exists so a resolve can ask the cheap
//! question ("are the four blobs I already hold still the four blobs the source is
//! publishing?") instead of the expensive one ("give me all sixteen blobs again"), which
//! is what [`IndexCache::load_if_identical`] answers.
//!
//! It is a CHANGE DETECTOR, not a trust input, and the distinction is the whole safety
//! argument. A matching identity does not make cached bytes trusted — they are handed to
//! [`crate::select::select_index`] exactly as freshly-downloaded bytes are, and face the
//! same master-signed roster admission, the same machine-signature check, the same
//! durable floor and the same `valid_until` window. A NON-matching identity is not a
//! failure either: it simply falls through to the historical download. So the worst a
//! wrong identity can do — a collision, a host that froze it, a locally-tampered cache —
//! is serve bytes that were already published and already pass every gate, i.e. exactly
//! the suppression an authority serving the index could always perform by withholding.

use std::fs;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

use crate::select::Candidate;

/// The one schema version this build writes and the ONLY one it reads. See the
/// fail-closed check in `read_doc`.
const CACHE_SCHEMA: u32 = 1;
const MAX_CACHE_CANDIDATES: usize = 24;
const MAX_CACHED_INDEX_BYTES: usize = 5_000_000;
const MAX_CACHED_SIGNATURE_BYTES: usize = 4_096;
/// A roster is a few hundred bytes per machine and is capped at 16 machines; 64 KiB is a
/// ceiling, not a fit. Matches the download cap `net` and `aterm-update` both use.
const MAX_CACHED_ROSTER_BYTES: usize = 65_536;
/// A label is a release tag (`v1.4.2`, `index-2026-08-20`). Git's own ref format and
/// every filesystem that has to hold one keep these far under 255 bytes; 512 is a
/// ceiling, not a fit. Enforced on BOTH sides — see `store` and `read_doc`.
const MAX_CACHED_LABEL_BYTES: usize = 512;
const MAX_INDEX_CACHE_BYTES: usize = 144 * 1024 * 1024;
/// Bound on one entry's `identity` string. The production fingerprint is a 64-char
/// hex digest ([`crate::net`]); the ceiling is generous so a future fetcher can use a
/// longer form, and small enough that a hostile cache cannot spend the document budget
/// on identities alone.
const MAX_CACHED_IDENTITY_BYTES: usize = 256;

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
    /// The fetcher-computed fingerprint of the release assets these bytes came from.
    /// `#[serde(default)]` for the same reason the roster fields are: a cache written
    /// by an older build still DECODES, and decodes to an EMPTY identity, which
    /// [`IndexCache::load_if_identical`] refuses to match — so a legacy cache keeps
    /// working as the failure-only fallback it always was, and is upgraded in place by
    /// the next successful fetch. EMPTY IS NEVER A MATCH.
    #[serde(default)]
    identity: String,
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

    /// Persist `candidates` tagged with `source_id`, each stamped with the matching
    /// entry of `identities` (the fetcher's fingerprint of the assets the bytes came
    /// from; pass `&[]` when the fetcher cannot supply one). Never clobbers a good cache
    /// with an empty "success" (an empty candidate set is not stored). Best-effort — any
    /// error is swallowed so a cache-write failure never fails an install. Written `0600`
    /// via temp + rename so a reader never sees a half-written cache.
    ///
    /// The identities are stored ONLY when they line up 1:1 with the bytes they describe
    /// and every one is present and bounded. Anything else stores EMPTY identities, which
    /// no [`IndexCache::load_if_identical`] can match — the optimization simply does not
    /// engage, and the entry stays a perfectly good failure-time fallback. That guard is
    /// the binding: an identity that described some OTHER candidate set is the one way
    /// this could serve bytes the source is no longer publishing, so a mismatch in COUNT
    /// (the only way the pairing can silently slip) refuses the whole vector rather than
    /// zipping a prefix.
    pub fn store(&self, source_id: &str, candidates: &[Candidate], identities: &[String]) {
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
        // Pair-or-nothing (see the doc above): an identity vector that does not line up
        // exactly with `candidates` is dropped WHOLE, so `paired.get(i)` can only ever yield
        // the fingerprint of the very bytes on the same row.
        let paired: &[String] = if identities.len() == candidates.len()
            && identities
                .iter()
                .all(|id| !id.is_empty() && id.len() <= MAX_CACHED_IDENTITY_BYTES)
        {
            identities
        } else {
            &[]
        };
        let doc = CacheDoc {
            schema: CACHE_SCHEMA,
            source: source_id.to_string(),
            fetched_at: String::new(),
            candidate: candidates
                .iter()
                .enumerate()
                .map(|(i, c)| CachedCandidate {
                    label: c.label.clone(),
                    index_b64: STANDARD.encode(&c.index_bytes),
                    sig_b64: STANDARD.encode(&c.sig),
                    roster_b64: STANDARD.encode(&c.roster_bytes),
                    roster_sig_b64: STANDARD.encode(&c.roster_sig),
                    identity: paired.get(i).cloned().unwrap_or_default(),
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
        decode(self.read_doc(source_id)?)
    }

    /// The HIT PATH: the cached candidates IFF the same-source guard passes AND every
    /// entry's stored `identity` equals `live` — the identity of the candidate set the
    /// source is publishing RIGHT NOW, in the same order.
    ///
    /// # Why this is exactly as safe as the failure-time fallback
    ///
    /// It returns the same kind of thing `load` returns: RAW BYTES. They go to
    /// [`crate::select::select_index`] unchanged, so the master-signed roster admission,
    /// the machine signature over the index, the durable `index_build` floor, the
    /// `roster_seq` ratchet and the `valid_until` freshness window all run identically.
    /// Nothing here decides that anything is trusted; it decides only that re-downloading
    /// sixteen assets to obtain bytes we can prove are the same sixteen assets is work
    /// with no product.
    ///
    /// Refuses — i.e. falls through to the download — on EVERY ambiguity: an empty `live`
    /// (a fetcher that cannot answer cheaply), a different candidate COUNT, any entry
    /// whose stored identity is empty (a legacy cache), and any positional mismatch. The
    /// failure direction is "pay the historical cost", never "serve the wrong bytes".
    #[must_use]
    pub fn load_if_identical(&self, source_id: &str, live: &[String]) -> Option<Vec<Candidate>> {
        if live.is_empty() {
            return None;
        }
        let doc = self.read_doc(source_id)?;
        if doc.candidate.len() != live.len() {
            return None;
        }
        // ORDER MATTERS. `index_candidates` yields candidates newest-first and the
        // identity vector is built from the same walk in the same order, so comparing
        // positionally is comparing like with like; a set that merely PERMUTED would be a
        // different publication state and is refused.
        if doc
            .candidate
            .iter()
            .zip(live)
            .any(|(c, id)| c.identity.is_empty() || c.identity.as_str() != id.as_str())
        {
            return None;
        }
        decode(doc)
    }

    /// Read + parse the cache document, applying the same-source guard, the candidate
    /// count bound and the per-entry identity ceiling. Shared by both readers so they
    /// cannot drift on what a readable, admissible cache document is.
    fn read_doc(&self, source_id: &str) -> Option<CacheDoc> {
        let text = crate::metadata_io::read_bounded_regular_utf8(&self.path, MAX_INDEX_CACHE_BYTES)
            .ok()?;
        let doc: CacheDoc = toml::from_str(&text).ok()?;
        if doc.schema != CACHE_SCHEMA {
            // FAIL CLOSED on an unrecognized version. A version field that fails OPEN is
            // the wrong direction for this document in particular: it has already needed
            // one compatibility migration (the roster fields), and that migration was
            // carried by `#[serde(default)]` at the FIELD level precisely so the schema
            // number would not have to move. So a schema this build does not recognize is
            // not an older cache to be read leniently — it is a document written by rules
            // this build does not have, and the only safe reading of it is none. The cost
            // of refusing is one re-fetch; the cost of accepting is honoring bytes under
            // the wrong rules.
            return None;
        }
        if doc.source != source_id || doc.candidate.len() > MAX_CACHE_CANDIDATES {
            return None; // SAME-SOURCE GUARD: a dir: cache never satisfies a github: fetch
        }
        // These are READ-path bounds, not merely the write-path ones in `store`: the
        // hostile document is by definition one we did NOT write, so a ceiling checked
        // only on the way out is checked in the one place it cannot matter. Every other
        // per-entry field is bounded in `decode`, which would leave `identity` and `label`
        // as the only unbounded strings in the document — MAX_CACHE_CANDIDATES of them
        // could then spend the whole MAX_INDEX_CACHE_BYTES budget on those two fields
        // alone, which is exactly what that constant's doc comment promises is impossible.
        // `label` matters most: it is the one that survives decoding into the returned
        // candidate. Both are checked HERE rather than in `decode` so the refusal costs no
        // base64 work, and so it covers `load` and `load_if_identical` alike.
        // Fail-closed on the WHOLE document, as an oversize blob already does.
        if doc.candidate.iter().any(|c| {
            c.identity.len() > MAX_CACHED_IDENTITY_BYTES || c.label.len() > MAX_CACHED_LABEL_BYTES
        }) {
            return None;
        }
        Some(doc)
    }
}

/// Decode a vetted cache document into raw candidates, enforcing the per-blob byte
/// ceilings. Any decode failure or oversize blob fails the WHOLE document (`None`) — a
/// half-decoded candidate set is not a cache, it is a selection input nobody chose.
fn decode(doc: CacheDoc) -> Option<Vec<Candidate>> {
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
        c.store("github:o/r", &[cand("v1"), cand("v0")], &[]);
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
        c.store("dir:/tmp/reg", &[cand("v1")], &[]);
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
        c.store("github:o/r", &[cand("v1")], &[]); // good cache
        c.store("github:o/r", &[], &[]); // an empty "success" must NOT clobber it
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
        cache.store("any", &candidates, &[]);
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
        IndexCache::new(target.clone()).store("any", &[cand("one")], &[]);
        std::os::unix::fs::symlink(&target, &p).unwrap();
        assert!(IndexCache::new(p.clone()).load("any").is_none());
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    /// THE HIT PATH, both directions. A cache stamped with identities is served WITHOUT
    /// any fetch when the live identities match, and refuses the moment ANY of them
    /// moves — which is the whole safety argument: the reuse is conditional on the
    /// source still publishing the very assets these bytes came from.
    #[test]
    fn identical_identities_serve_the_cache_and_any_drift_refuses_it() {
        let p = tmp("identity");
        let c = IndexCache::new(p.clone());
        let ids = vec!["id-v1".to_string(), "id-v0".to_string()];
        c.store("github:o/r", &[cand("v1"), cand("v0")], &ids);

        let hit = c
            .load_if_identical("github:o/r", &ids)
            .expect("matching identities serve the cached bytes");
        assert_eq!(hit.len(), 2);
        assert_eq!(hit[0].label, "v1");
        assert_eq!(hit[0].index_bytes, vec![1, 2, 3, 0xFF]);
        assert_eq!(hit[0].roster_bytes, vec![0xDE, 0xAD, 0xBE, 0xEF]);

        // A re-uploaded asset moves ONE identity: refuse, and re-download.
        let moved = vec!["id-v1-NEW".to_string(), "id-v0".to_string()];
        assert!(c.load_if_identical("github:o/r", &moved).is_none());
        // A newly published release changes the COUNT: refuse.
        let grown = vec![
            "id-v2".to_string(),
            "id-v1".to_string(),
            "id-v0".to_string(),
        ];
        assert!(c.load_if_identical("github:o/r", &grown).is_none());
        // A permutation is a different publication state: refuse.
        let swapped = vec!["id-v0".to_string(), "id-v1".to_string()];
        assert!(c.load_if_identical("github:o/r", &swapped).is_none());
        // A fetcher that cannot answer cheaply (empty live vector): refuse.
        assert!(c.load_if_identical("github:o/r", &[]).is_none());
        // And the SAME-SOURCE guard still outranks a perfect identity match.
        assert!(c.load_if_identical("dir:/elsewhere", &ids).is_none());
        // The failure-time fallback is unchanged by any of this.
        assert_eq!(c.load("github:o/r").unwrap().len(), 2);
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    /// The pair-or-nothing guard. An identity vector that does not line up 1:1 with the
    /// candidates is dropped WHOLE — never zipped as a prefix — so no entry can ever
    /// carry a fingerprint belonging to some other candidate's bytes.
    #[test]
    fn a_misaligned_identity_vector_is_dropped_whole() {
        let p = tmp("identity-misaligned");
        let c = IndexCache::new(p.clone());
        // Two candidates, ONE identity: the pairing is ambiguous, so nothing is stamped.
        c.store(
            "github:o/r",
            &[cand("v1"), cand("v0")],
            &["id-v1".to_string()],
        );
        assert!(
            c.load_if_identical("github:o/r", &["id-v1".to_string()])
                .is_none(),
            "a prefix must not be zipped onto the first row"
        );
        assert!(
            c.load("github:o/r").is_some(),
            "the entry is still a good failure-time fallback"
        );
        // An EMPTY identity is never a match either (this is also the legacy-cache case).
        let c2 = IndexCache::new(p.clone());
        c2.store("github:o/r", &[cand("v1")], &[String::new()]);
        assert!(
            c2.load_if_identical("github:o/r", &[String::new()])
                .is_none(),
            "empty is never a match, in either position"
        );
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    /// A cache written by a build that predates identities decodes, serves the historical
    /// failure-time fallback, and NEVER satisfies the hit path — so an in-place upgrade
    /// pays one ordinary resolve and is stamped from then on.
    #[test]
    fn a_pre_identity_cache_entry_never_matches_the_hit_path() {
        let p = tmp("identity-legacy");
        fs::write(
            &p,
            "schema = 1\nsource = \"github:o/r\"\nfetched_at = \"\"\n\
             [[candidate]]\nlabel = \"v1\"\nindex_b64 = \"AQID\"\nsig_b64 = \"CQgH\"\n\
             roster_b64 = \"3q2+7w==\"\nroster_sig_b64 = \"BAUG\"\n",
        )
        .unwrap();
        let c = IndexCache::new(p.clone());
        assert_eq!(
            c.load("github:o/r")
                .expect("legacy entry still decodes")
                .len(),
            1
        );
        assert!(
            c.load_if_identical("github:o/r", &["anything".to_string()])
                .is_none(),
            "no stored identity ⇒ no hit path"
        );
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    /// The identity ceiling binds on the way IN, not just on the way out.
    ///
    /// `store` refuses to stamp an oversize identity, but the document this bound exists
    /// to survive is one an attacker wrote, which never went through `store` at all.
    /// Every other per-entry field is bounded in `decode`, so an unchecked `identity`
    /// would be the only unbounded string in the document and `MAX_CACHE_CANDIDATES` of
    /// them could spend the entire `MAX_INDEX_CACHE_BYTES` budget — the precise thing the
    /// constant's doc comment promises cannot happen.
    ///
    /// TWO-SIDED on purpose: an identity exactly AT the ceiling must still work (or the
    /// bound is silently something other than what it says, and the production 64-char
    /// digest is one refactor away from being refused), and one byte OVER must refuse
    /// through BOTH readers.
    #[test]
    fn an_oversize_identity_is_refused_by_the_read_path() {
        let write_doc = |p: &PathBuf, id: &str| {
            fs::write(
                p,
                format!(
                    "schema = 1\nsource = \"github:o/r\"\nfetched_at = \"\"\n\
                     [[candidate]]\nlabel = \"v1\"\nindex_b64 = \"AQID\"\nsig_b64 = \"CQgH\"\n\
                     roster_b64 = \"3q2+7w==\"\nroster_sig_b64 = \"BAUG\"\n\
                     identity = \"{id}\"\n"
                ),
            )
            .unwrap();
        };

        // AT the ceiling: admissible, and a real hit.
        let p = tmp("identity-at-ceiling");
        let at = "a".repeat(MAX_CACHED_IDENTITY_BYTES);
        write_doc(&p, &at);
        let c = IndexCache::new(p.clone());
        assert!(
            c.load("github:o/r").is_some(),
            "an identity exactly at the ceiling is admissible"
        );
        assert!(
            c.load_if_identical("github:o/r", std::slice::from_ref(&at))
                .is_some(),
            "and still serves the hit path"
        );
        let _ = fs::remove_dir_all(p.parent().unwrap());

        // ONE BYTE OVER: refused by both readers, whole document.
        let p2 = tmp("identity-over-ceiling");
        let over = "a".repeat(MAX_CACHED_IDENTITY_BYTES + 1);
        write_doc(&p2, &over);
        let c2 = IndexCache::new(p2.clone());
        assert!(
            c2.load("github:o/r").is_none(),
            "one byte over the ceiling refuses the whole document"
        );
        assert!(
            c2.load_if_identical("github:o/r", &[over]).is_none(),
            "and a perfect match on an oversize identity is still no hit"
        );
        let _ = fs::remove_dir_all(p2.parent().unwrap());
    }

    /// Two-sided, BOTH readers, BOTH paths: exactly at the ceiling is a working cache, one
    /// byte over is refused by the writer and by `load`/`load_if_identical` alike.
    ///
    /// The read side is the half that matters. `store` only ever sees labels this build
    /// produced; the readers parse a document we did NOT write, and `label` is the one
    /// field that survives decoding into the returned candidate.
    #[test]
    fn label_ceiling_is_enforced_on_both_paths() {
        let write_doc = |p: &PathBuf, label: &str| {
            fs::write(
                p,
                format!(
                    "schema = 1\nsource = \"github:o/r\"\nfetched_at = \"\"\n\
                     [[candidate]]\nlabel = \"{label}\"\nindex_b64 = \"AQID\"\n\
                     sig_b64 = \"CQgH\"\nroster_b64 = \"3q2+7w==\"\n\
                     roster_sig_b64 = \"BAUG\"\nidentity = \"id1\"\n"
                ),
            )
            .unwrap();
        };

        // AT the ceiling: admissible through both readers, and the label survives intact.
        let p = tmp("label-at-ceiling");
        let at = "L".repeat(MAX_CACHED_LABEL_BYTES);
        write_doc(&p, &at);
        let c = IndexCache::new(p.clone());
        let got = c.load("github:o/r").expect("a label at the ceiling is admissible");
        assert_eq!(got[0].label, at, "and reaches the caller unmodified");
        assert!(
            c.load_if_identical("github:o/r", &["id1".to_string()])
                .is_some(),
            "and still serves the hit path"
        );
        let _ = fs::remove_dir_all(p.parent().unwrap());

        // ONE BYTE OVER: refused by both readers, whole document.
        let p2 = tmp("label-over-ceiling");
        write_doc(&p2, &"L".repeat(MAX_CACHED_LABEL_BYTES + 1));
        let c2 = IndexCache::new(p2.clone());
        assert!(
            c2.load("github:o/r").is_none(),
            "one byte over the ceiling refuses the whole document"
        );
        assert!(
            c2.load_if_identical("github:o/r", &["id1".to_string()])
                .is_none(),
            "and a perfect identity match on an oversize label is still no hit"
        );
        let _ = fs::remove_dir_all(p2.parent().unwrap());

        // WRITE path: an oversize label is never written, and never clobbers a good cache.
        let p3 = tmp("label-write");
        fs::write(&p3, "sentinel").unwrap();
        IndexCache::new(p3.clone()).store(
            "src",
            &[cand(&"L".repeat(MAX_CACHED_LABEL_BYTES + 1))],
            &[],
        );
        assert_eq!(
            fs::read_to_string(&p3).unwrap(),
            "sentinel",
            "an oversize label must not overwrite a good cache"
        );
        let _ = fs::remove_dir_all(p3.parent().unwrap());
    }

    /// An unrecognized schema is refused rather than read leniently — and the version this
    /// build writes is, of course, still read, by both readers.
    #[test]
    fn unknown_schema_fails_closed() {
        let p = tmp("schema");
        IndexCache::new(p.clone()).store("src", &[cand("v1")], &["id1".to_string()]);
        let c = IndexCache::new(p.clone());
        assert!(
            c.load("src").is_some(),
            "the schema this build writes must still load — no behaviour change for valid documents"
        );
        assert!(
            c.load_if_identical("src", &["id1".to_string()]).is_some(),
            "and the hit path is unaffected too"
        );

        let good = fs::read_to_string(&p).unwrap();
        assert!(
            good.contains(&format!("schema = {CACHE_SCHEMA}")),
            "the writer stamps the constant, so the two sides cannot drift"
        );
        for bad in ["0", "2", "4294967295"] {
            fs::write(
                &p,
                good.replace(&format!("schema = {CACHE_SCHEMA}"), &format!("schema = {bad}")),
            )
            .unwrap();
            let c = IndexCache::new(p.clone());
            assert!(
                c.load("src").is_none(),
                "schema {bad} is not a version this build knows; it must fail CLOSED"
            );
            assert!(
                c.load_if_identical("src", &["id1".to_string()]).is_none(),
                "and the hit path must not read it either"
            );
        }
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

}
