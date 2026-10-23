// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Same-source index cache for fallback on transient fetch failure.

use std::fs;
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
        let tmp = parent.join(format!(".index-cache.tmp-{}", std::process::id()));
        if fs::write(&tmp, text.as_bytes()).is_err() {
            let _ = fs::remove_file(&tmp);
            return;
        }
        let _ = crate::platform::harden_file(&tmp);
        if fs::rename(&tmp, &self.path).is_err() {
            let _ = fs::remove_file(&tmp);
        }
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
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn tmp(label: &str) -> (crate::testdir::Dir, PathBuf) {
        let d = crate::testdir::dir(&format!("atpkg-cache-{label}-"));
        let file = d.join("index-cache.toml");
        (d, file)
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
        let (_scratch, p) = tmp("rt");
        let c = IndexCache::new(p.clone());
        c.store("github:o/r", &[cand("v1"), cand("v0")]);
        let back = c.load("github:o/r").expect("same source loads");
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].label, "v1");
        assert_eq!(back[0].index_bytes, vec![1, 2, 3, 0xFF]);
        assert_eq!(back[0].sig, vec![9, 8, 7]);
        assert_eq!(back[0].roster_bytes, vec![0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(back[0].roster_sig, vec![4, 5, 6]);
        #[cfg(unix)]
        {
            let mode = fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn load_rejects_other_source() {
        let (_scratch, p) = tmp("other");
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
    fn a_pre_roster_cache_entry_decodes_but_authorizes_nothing() {
        let (_scratch, p) = tmp("legacy");
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
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn load_none_on_missing_or_garbage() {
        let (_scratch, p) = tmp("garbage");
        let c = IndexCache::new(p.clone());
        assert!(c.load("any").is_none(), "absent cache is None");
        fs::write(&p, "this is not valid toml {{{").unwrap();
        assert!(c.load("any").is_none(), "garbage cache is fail-closed None");
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn store_skips_empty_candidate_set() {
        let (_scratch, p) = tmp("empty");
        let c = IndexCache::new(p.clone());
        c.store("github:o/r", &[cand("v1")]); // good cache
        c.store("github:o/r", &[]); // an empty "success" must not clobber it
        assert_eq!(
            c.load("github:o/r").unwrap().len(),
            1,
            "good cache preserved"
        );
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn sparse_oversized_and_overcount_cache_fail_closed() {
        let (_scratch, p) = tmp("bounded");
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

        let (_scratch, p) = tmp("special");
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

    #[test]
    fn a_store_that_cannot_land_leaves_no_temp_behind() {
        let (_scratch, p) = tmp("tmp-leak");
        let parent = p.parent().unwrap().to_path_buf();
        fs::create_dir(&p).unwrap();
        let c = IndexCache::new(p.clone());
        c.store("github:o/r", &[cand("v1")]);
        assert!(
            p.is_dir(),
            "the un-renamable cache path is left exactly as it was"
        );
        let strays: Vec<String> = fs::read_dir(&parent)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".index-cache.tmp-"))
            .collect();
        assert!(
            strays.is_empty(),
            "a store that cannot land must not strand its temp at the prefix root: {strays:?}"
        );
        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn label_ceiling_is_enforced_on_both_paths() {
        let write_doc = |p: &PathBuf, label: &str| {
            fs::write(
                p,
                format!(
                    "schema = 1\nsource = \"github:o/r\"\nfetched_at = \"\"\n\
                     [[candidate]]\nlabel = \"{label}\"\nindex_b64 = \"AQID\"\n\
                     sig_b64 = \"CQgH\"\nroster_b64 = \"3q2+7w==\"\n\
                     roster_sig_b64 = \"BAUG\"\n"
                ),
            )
            .unwrap();
        };

        let (_scratch, p) = tmp("label-at-ceiling");
        let at = "L".repeat(MAX_CACHED_LABEL_BYTES);
        write_doc(&p, &at);
        let c = IndexCache::new(p.clone());
        let got = c
            .load("github:o/r")
            .expect("a label at the ceiling is admissible");
        assert_eq!(got[0].label, at, "and reaches the caller unmodified");
        let _ = fs::remove_dir_all(p.parent().unwrap());

        let (_scratch, p2) = tmp("label-over-ceiling");
        write_doc(&p2, &"L".repeat(MAX_CACHED_LABEL_BYTES + 1));
        let c2 = IndexCache::new(p2.clone());
        assert!(
            c2.load("github:o/r").is_none(),
            "one byte over the ceiling refuses the whole document"
        );
        let _ = fs::remove_dir_all(p2.parent().unwrap());

        let (_scratch, p3) = tmp("label-write");
        fs::write(&p3, "sentinel").unwrap();
        IndexCache::new(p3.clone()).store("src", &[cand(&"L".repeat(MAX_CACHED_LABEL_BYTES + 1))]);
        assert_eq!(
            fs::read_to_string(&p3).unwrap(),
            "sentinel",
            "an oversize label must not overwrite a good cache"
        );
        let _ = fs::remove_dir_all(p3.parent().unwrap());
    }

    #[test]
    fn unknown_schema_fails_closed() {
        let (_scratch, p) = tmp("schema");
        IndexCache::new(p.clone()).store("src", &[cand("v1")]);
        let c = IndexCache::new(p.clone());
        assert!(
            c.load("src").is_some(),
            "the schema this build writes must still load — no behaviour change for valid documents"
        );

        let good = fs::read_to_string(&p).unwrap();
        assert!(
            good.contains(&format!("schema = {CACHE_SCHEMA}")),
            "the writer stamps the constant, so the two sides cannot drift"
        );
        for bad in ["0", "2", "4294967295"] {
            fs::write(
                &p,
                good.replace(
                    &format!("schema = {CACHE_SCHEMA}"),
                    &format!("schema = {bad}"),
                ),
            )
            .unwrap();
            let c = IndexCache::new(p.clone());
            assert!(
                c.load("src").is_none(),
                "schema {bad} is not a version this build knows; it must fail CLOSED"
            );
        }
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    #[cfg(unix)]
    fn mode_of(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    fn system_shaped_layout(label: &str) -> Layout {
        let layout = Layout {
            prefix: std::env::temp_dir().join(format!(
                "atpkg-cache-{label}-{}-never-created",
                std::process::id()
            )),
        };
        crate::store::system_shape::force(&layout.prefix);
        layout
    }

    #[cfg(unix)]
    #[test]
    fn a_system_shaped_prefix_leaves_the_cache_parent_traversable() {
        let layout = system_shaped_layout("sys");
        assert!(layout.is_system_prefix(), "the fixture is the system shape");
        let system_path = layout.prefix.clone();
        let (_scratch, p) = tmp("sys-parent");
        let parent = p.parent().unwrap().to_path_buf();
        fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cache = IndexCache {
            path: p.clone(),
            layout: Some(layout),
        };
        cache.store("github:o/r", &[cand("v1")]);
        assert!(
            cache.load("github:o/r").is_some(),
            "the cache is still written — this changes the DIRECTORY mode, nothing else"
        );
        assert_eq!(
            mode_of(&parent),
            0o755,
            "a system prefix's cache parent must stay traversable by every user, not root-only"
        );
        assert!(
            !system_path.exists(),
            "the fixture reads the chain and creates nothing under it"
        );
        let _ = fs::remove_dir_all(&parent);
    }

    #[cfg(unix)]
    #[test]
    fn a_home_shaped_prefix_keeps_the_cache_parent_private() {
        let (_scratch, p) = tmp("home-parent");
        let parent = p.parent().unwrap().to_path_buf();
        fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        let layout = Layout {
            prefix: parent.clone(),
        };
        assert!(
            !layout.is_system_prefix(),
            "a user-owned temp prefix is never the system shape"
        );
        let cache = IndexCache::for_layout(&layout);
        assert_eq!(
            cache.path, p,
            "for_layout addresses <prefix>/index-cache.toml"
        );
        cache.store("github:o/r", &[cand("v1")]);
        assert!(cache.load("github:o/r").is_some(), "the cache is written");
        assert_eq!(
            mode_of(&parent),
            0o700,
            "a $HOME prefix's cache parent is private state and stays 0700"
        );
        let _ = fs::remove_dir_all(&parent);
    }
}
