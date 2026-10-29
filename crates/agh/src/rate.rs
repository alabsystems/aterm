// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Rate-limit snapshots (`x-ratelimit-*`) and the advisory per-identity rate book.

use std::path::PathBuf;

use aterm_json::{Map, Value};

use crate::error::{Error, IdentityRef, Kind, Sub};

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

impl RateSnapshot {
    /// Parses `x-ratelimit-*` headers (`resource` defaults to `core`).
    #[must_use]
    pub fn from_headers(headers: &[(String, String)]) -> Option<RateSnapshot> {
        let get = |name: &str| crate::wire::header_in(headers, name).map(str::trim);
        let number = |name: &str| get(name).and_then(|v| v.parse::<u64>().ok());
        let limit = number("x-ratelimit-limit")?;
        let remaining = number("x-ratelimit-remaining")?;
        let reset = number("x-ratelimit-reset")?;
        Some(RateSnapshot {
            resource: get("x-ratelimit-resource")
                .filter(|s| !s.is_empty())
                .unwrap_or("core")
                .to_string(),
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
            resource: value.get("resource")?.as_str()?.to_string(),
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
pub fn book_key(identity: &IdentityRef) -> String {
    identity.fingerprint().unwrap_or("anonymous").to_string()
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
        let spent = self
            .read(key)
            .into_iter()
            .find(|s| s.resource == resource && s.remaining == 0 && s.reset > now_unix)?;
        Some(
            Error::new(
                Kind::RateLimited,
                format!(
                    "the rate book says {resource} is spent for this identity until {}",
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
        let parent = dir.parent()?;
        if let Some(grand) = parent.parent() {
            std::fs::create_dir_all(grand).ok()?;
        }
        create_private_dir(parent).ok()?;
        create_private_dir(dir).ok()?;
        let mut held = self.read(key);
        held.retain(|s| s.resource != snapshot.resource);
        held.push(snapshot.clone());
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
            .values()
            .filter_map(RateSnapshot::from_json)
            .collect();
        held.sort_by(|a, b| a.resource.cmp(&b.resource));
        Some(held)
    }
}
