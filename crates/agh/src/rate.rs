// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Rate-limit snapshots (`x-ratelimit-*`) and the advisory per-identity rate book.

use std::path::PathBuf;

use aterm_json::{Map, Value};

use crate::error::{Error, IdentityRef, Kind, Sub};

mod usage;

/// One resource's limit as GitHub last reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateSnapshot {
    /// `core`, `search`, `graphql`, ….
    pub resource: String,
    /// The limit.
    pub limit: u64,
    /// What remains.
    pub remaining: u64,
    /// When it resets, in Unix seconds.
    pub reset: u64,
    /// What is used.
    pub used: u64,
}

/// Maximum length of a normalized rate-limit resource name.
const RESOURCE_MAX_LEN: usize = 64;

/// Maximum distinct resources retained per identity in [`RateBook`].
#[cfg(unix)]
const MAX_RESOURCES: usize = 64;

/// Normalizes a rate-limit resource name to lowercase `[a-z0-9_-]{1,64}`.
fn normalize_resource_cow(raw: &str) -> Option<std::borrow::Cow<'_, str>> {
    let raw = raw.trim();
    if raw.is_empty()
        || raw.len() > RESOURCE_MAX_LEN
        || !raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return None;
    }
    if raw.bytes().any(|b| b.is_ascii_uppercase()) {
        Some(std::borrow::Cow::Owned(raw.to_ascii_lowercase()))
    } else {
        Some(std::borrow::Cow::Borrowed(raw))
    }
}

fn normalize_resource(raw: &str) -> Option<String> {
    normalize_resource_cow(raw).map(std::borrow::Cow::into_owned)
}

impl RateSnapshot {
    /// Parses `x-ratelimit-*` headers (`resource` defaults to `core`, normalized to lowercase).
    #[must_use]
    pub fn from_headers(headers: &[(String, String)]) -> Option<RateSnapshot> {
        let get = |name: &str| crate::wire::header_in(headers, name).map(str::trim);
        let number = |name: &str| get(name).and_then(crate::path::digits::<u64>);
        let limit = number("x-ratelimit-limit")?;
        let remaining = number("x-ratelimit-remaining")?;
        let reset = number("x-ratelimit-reset")?;
        let resource = match get("x-ratelimit-resource") {
            None | Some("") => "core".to_string(),
            Some(raw) => normalize_resource(raw)?,
        };
        Some(RateSnapshot {
            resource,
            limit,
            remaining,
            reset,
            used: number("x-ratelimit-used").unwrap_or(limit.saturating_sub(remaining)),
        })
    }

    /// `{"limit","remaining","reset","resource","used"}`.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("limit".into(), Value::from(self.limit));
        object.insert("remaining".into(), Value::from(self.remaining));
        object.insert("reset".into(), Value::from(self.reset));
        object.insert("resource".into(), Value::from(self.resource.as_str()));
        object.insert("used".into(), Value::from(self.used));
        Value::Object(object)
    }

    /// The snapshot `to_json` wrote.
    #[must_use]
    pub fn from_json(value: &Value) -> Option<RateSnapshot> {
        let number = |key: &str| value.get(key).and_then(Value::as_u64);
        Some(RateSnapshot {
            resource: normalize_resource(value.get("resource")?.as_str()?)?,
            limit: number("limit")?,
            remaining: number("remaining")?,
            reset: number("reset")?,
            used: number("used")?,
        })
    }

    /// `core:4987/5000`.
    #[must_use]
    pub fn line(&self) -> String {
        format!("{}:{}/{}", self.resource, self.remaining, self.limit)
    }
}

/// Returns the GitHub API rate-limit resource (`code_search`, `search`, `graphql`, or `core`).
#[inline]
#[must_use]
pub fn resource_for(path: &str) -> &'static str {
    if path == "/search/code" || path.starts_with("/search/code?") {
        "code_search"
    } else if path.starts_with("/search/") {
        "search"
    } else if path == "/graphql" || path.starts_with("/graphql?") {
        "graphql"
    } else {
        "core"
    }
}

/// The identity's fingerprint, or `anonymous`.
#[inline]
#[must_use]
pub(crate) fn book_key_str(identity: &IdentityRef) -> &str {
    identity.fingerprint().unwrap_or("anonymous")
}

/// The identity's fingerprint, or `anonymous`.
#[inline]
#[must_use]
pub fn book_key(identity: &IdentityRef) -> String {
    book_key_str(identity).to_string()
}

/// The advisory per-identity rate book: `<dir>/<key>.json`, `{"v":1,"resources":{…}}`.
#[derive(Clone, Debug)]
pub struct RateBook {
    dir: Option<PathBuf>,
}

/// The longest rate-book file read.
#[cfg(unix)]
const BOOK_MAX: usize = 64 * 1024;

/// The book file's name for `key`.
#[cfg(unix)]
fn file_name(key: &str) -> String {
    format!("{key}.json")
}

/// A key that names one file directly in the book's directory: a fingerprint or `anonymous`.
#[cfg(unix)]
fn usable_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl RateBook {
    /// The book in `dir` (`Dirs::rate()`).
    #[inline]
    #[must_use]
    pub const fn at(dir: PathBuf) -> RateBook {
        RateBook { dir: Some(dir) }
    }

    /// No book.
    #[inline]
    #[must_use]
    pub const fn none() -> RateBook {
        RateBook { dir: None }
    }

    /// Whether the rate book is active on this platform (`unix` with a configured directory).
    #[inline]
    #[must_use]
    pub(crate) const fn enabled(&self) -> bool {
        cfg!(unix) && self.dir.is_some()
    }

    /// Returns a `PreRefused` rate-limit error when `resource` is exhausted until after `now_unix`.
    #[must_use]
    pub fn refuse_before_send(&self, key: &str, resource: &str, now_unix: u64) -> Option<Error> {
        let normalized = normalize_resource_cow(resource)?;
        let spent = self
            .read(key)
            .into_iter()
            .find(|s| s.resource == *normalized && s.remaining == 0 && s.reset > now_unix)?;
        Some(
            Error::new(
                Kind::RateLimited,
                format!(
                    "the rate book says {normalized} is spent for this identity until {}",
                    spent.reset
                ),
            )
            .with_sub(Sub::PreRefused)
            .with_reset(spent.reset)
            .with_rate(spent),
        )
    }

    /// Merges `snapshot` into `key`'s advisory rate-book file, ignoring I/O errors.
    pub fn record(&self, key: &str, snapshot: &RateSnapshot) {
        #[cfg(unix)]
        {
            let _ = self.try_record(key, snapshot);
        }
        #[cfg(not(unix))]
        {
            let _ = (&self.dir, key, snapshot);
        }
    }

    /// `record`'s steps, stopping at the first failure.
    #[cfg(unix)]
    fn try_record(&self, key: &str, snapshot: &RateSnapshot) -> Option<()> {
        use aterm_uds::private_file::{create_private_dir, write_private};
        let dir = self.dir.as_deref()?;
        if !usable_key(key) {
            return None;
        }
        let resource = normalize_resource(&snapshot.resource)?;
        let normalized = RateSnapshot {
            resource,
            ..snapshot.clone()
        };
        if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            if let Some(grand) = parent.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(grand).ok()?;
            }
            create_private_dir(parent).ok()?;
        }
        create_private_dir(dir).ok()?;
        let mut held = self.read(key);
        held.retain(|s| s.resource != normalized.resource);
        while held.len() >= MAX_RESOURCES {
            // Evict unspent entries before spent (`remaining == 0`) ones so synthetic unspent
            // resource churn cannot displace an active `PreRefused` guard.
            let evict_idx = held
                .iter()
                .enumerate()
                .min_by_key(|(_, s)| (s.remaining == 0, s.reset))
                .map(|(idx, _)| idx)?;
            held.swap_remove(evict_idx);
        }
        held.push(normalized);
        let mut resources = Map::new();
        for s in &held {
            resources.insert(s.resource.clone(), s.to_json());
        }
        let mut book = Map::new();
        book.insert("resources".into(), Value::Object(resources));
        book.insert("v".into(), Value::from(1u64));
        let text = aterm_json::to_string(&Value::Object(book)).ok()?;
        write_private(dir, &file_name(key), text.as_bytes()).ok()
    }

    /// Reads `key`'s snapshots sorted by resource, or an empty list if unreadable.
    #[must_use]
    pub fn read(&self, key: &str) -> Vec<RateSnapshot> {
        #[cfg(unix)]
        {
            self.try_read(key).unwrap_or_default()
        }
        #[cfg(not(unix))]
        {
            let _ = (&self.dir, key);
            Vec::new()
        }
    }

    /// `read`, or `None` at the first failure.
    #[cfg(unix)]
    fn try_read(&self, key: &str) -> Option<Vec<RateSnapshot>> {
        let dir = self.dir.as_deref()?;
        if !usable_key(key) {
            return None;
        }
        let bytes = aterm_uds::private_file::read_private(dir, &file_name(key), BOOK_MAX).ok()?;
        let value: Value = aterm_json::from_slice(&bytes).ok()?;
        if value.get("v").and_then(Value::as_u64) != Some(1) {
            return None;
        }
        let resources = value.get("resources")?.as_object()?;
        let mut held: Vec<RateSnapshot> = resources
            .iter()
            .filter_map(|(name, entry)| {
                let expected = normalize_resource_cow(name)?;
                let snap = RateSnapshot::from_json(entry)?;
                (snap.resource == *expected && name == &*expected).then_some(snap)
            })
            .take(MAX_RESOURCES)
            .collect();
        held.sort_unstable_by(|a, b| a.resource.cmp(&b.resource));
        Some(held)
    }
}

#[cfg(test)]
#[path = "rate_tests.rs"]
mod tests;
