// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared conditional response cache, separate from the body-free request journal.
//! Every reuse requires a fresh authenticated 304; no TTL permits a stale read.

use super::RateBook;
use crate::client::Answered;

/// One cached representation. Only response metadata needed to interpret its body survives.
pub(crate) struct Cached {
    pub etag: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[cfg(unix)]
const ENTRY_MAX: usize = 256 * 1024;
#[cfg(unix)]
const ENTRIES_MAX: usize = 128;

impl RateBook {
    /// Identity, origin, path/query and Accept are hashed together by the caller.
    pub(crate) fn cached(&self, key: &str, limit: u64) -> Option<Cached> {
        #[cfg(unix)]
        {
            let dir = self.dir.as_ref()?.join("responses");
            let bytes = aterm_uds::private_file::read_private(&dir, key, ENTRY_MAX).ok()?;
            let value: aterm_json::Value = aterm_json::from_slice(&bytes).ok()?;
            let body = aterm_codec::base64::decode(value["body"].as_str()?).ok()?;
            if body.len() as u64 > limit {
                return None;
            }
            let etag = value["etag"].as_str()?.to_owned();
            let headers = value["headers"]
                .as_array()?
                .iter()
                .map(|h| Some((h[0].as_str()?.to_owned(), h[1].as_str()?.to_owned())))
                .collect::<Option<Vec<_>>>()?;
            Some(Cached {
                etag,
                headers,
                body,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (key, limit);
            None
        }
    }

    /// Cache at most 128 small representations in owner-only files (32 MiB total).
    /// Storage failure loses an optimization; it never changes a successful read.
    pub(crate) fn cache(&self, key: &str, answer: &Answered) {
        #[cfg(unix)]
        let _ = (|| -> Option<()> {
            use aterm_uds::private_file::{PrivateLock, create_private_dir, write_private};
            let root = self.dir.as_ref()?;
            let etag = answer.etag.as_deref()?;
            if answer.status != 200
                || answer.body.len() > ENTRY_MAX / 2
                || answer.header("cache-control").is_some_and(|s| {
                    s.split(',')
                        .any(|s| s.trim().eq_ignore_ascii_case("no-store"))
                })
            {
                return None;
            }
            // record_exchange has already created the rate directory for this answer.
            let dir = root.join("responses");
            create_private_dir(&dir).ok()?;
            let headers: Vec<_> = answer
                .headers
                .iter()
                .filter(|(name, _)| {
                    matches!(
                        name.to_ascii_lowercase().as_str(),
                        "etag" | "link" | "content-type"
                    )
                })
                .cloned()
                .collect();
            let bytes = aterm_json::to_vec(&aterm_json::json!({"etag":etag,"headers":headers,"body":aterm_codec::base64::encode(&answer.body).ok()?})).ok()?;
            if bytes.len() > ENTRY_MAX {
                return None;
            }
            let _lock = PrivateLock::take(
                &dir,
                "cache.lock",
                std::time::Duration::from_secs(60),
                "conditional response cache is still locked after 60 seconds",
            )
            .ok()?;
            let mut files: Vec<_> = std::fs::read_dir(&dir)
                .ok()?
                .filter_map(Result::ok)
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                })
                .collect();
            files.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
            while files.len() >= ENTRIES_MAX {
                let oldest = files.remove(0);
                std::fs::remove_file(oldest.path()).ok()?;
            }
            write_private(&dir, key, &bytes).ok()?;
            Some(())
        })();
        #[cfg(not(unix))]
        let _ = (key, answer);
    }
}
