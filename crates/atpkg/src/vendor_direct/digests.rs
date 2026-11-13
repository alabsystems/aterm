// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! First-seen authenticated payload digest log per `(program, version, triple)`.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::durable;
use super::stamp::vendor_dir;
use super::table::is_vendor;
use super::version::Version;
use crate::manifest::{DigestAlg, one_digest};
use crate::store::Layout;

const LOG_SCHEMA: u32 = 1;

/// Entries kept per program: the highest versions win.
pub(crate) const KEEP_PER_PROGRAM: usize = 64;

const MAX_LOG_BYTES: usize = 128 * 1024;

/// A payload digest from documents the program's anchor checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuthenticatedDigest {
    alg: DigestAlg,
    hex: String,
}

impl AuthenticatedDigest {
    /// `hex` (one `alg` digest, either case), which the anchor has just authenticated.
    #[must_use]
    pub(crate) fn authenticated(alg: DigestAlg, hex: &str) -> Option<Self> {
        let hex = hex.to_ascii_lowercase();
        alg.is_lower_hex(&hex).then_some(Self { alg, hex })
    }

    /// One read back from a record an earlier pass wrote when its anchor vouched for it.
    #[must_use]
    pub(crate) fn recorded(sha256: &str, sha512: &str) -> Option<Self> {
        let (alg, hex) = one_digest(sha256, sha512)?;
        alg.is_lower_hex(hex).then(|| Self {
            alg,
            hex: hex.to_string(),
        })
    }

    /// The algorithm.
    #[must_use]
    pub(crate) const fn alg(&self) -> DigestAlg {
        self.alg
    }

    /// The digest, lowercase hex.
    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.hex
    }

    /// The `(sha256, sha512)` pair of keys a record writes it under.
    #[must_use]
    pub(crate) fn keyed(&self) -> (String, String) {
        match self.alg {
            DigestAlg::Sha256 => (self.hex.clone(), String::new()),
            DigestAlg::Sha512 => (String::new(), self.hex.clone()),
        }
    }
}

/// What [`record_or_conflict`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DigestSeen {
    /// First sighting; recorded when asked to.
    New,
    /// The same digest was seen before.
    Same,
    /// A different digest was seen first for this key; the log keeps it.
    Conflict {
        /// The first-seen digest.
        first: AuthenticatedDigest,
    },
}

#[derive(Serialize, Deserialize)]
struct LogFile {
    schema: u32,
    #[serde(default)]
    seen: Vec<Entry>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    program: String,
    version: String,
    triple: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    sha256: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    sha512: String,
}

/// `(program, version, triple, algorithm)`: a digest under one algorithm is first seen apart
/// from one under another, so a row whose anchor moved to another algorithm (grok's, from a
/// measured sha256 to npm's signed sha512) starts its sightings afresh.
type Key = (String, Version, String, DigestAlg);

const SHARED_LOG: [&str; 2] = ["claude", "codex"];

/// `program`'s log: `<prefix>/vendor/digests.toml` for a [`SHARED_LOG`] program, else
/// `<prefix>/vendor/digests-<program>.toml`.
#[must_use]
pub(crate) fn digests_path(layout: &Layout, program: &str) -> PathBuf {
    vendor_dir(layout).join(if SHARED_LOG.contains(&program) {
        String::from("digests.toml")
    } else {
        format!("digests-{program}.toml")
    })
}

/// Check `digest` against the first digest of its algorithm seen for `(program, version,
/// triple)` and, when `record`, record it if none was.
/// # Errors
/// An argument is malformed (not a vendor program, or a triple that is not 2 to 5 `[a-z0-9_.]`
/// words joined by `-`, at most 64 bytes), the log exists but does not read, or it could not be
/// locked or written.
pub(crate) fn record_or_conflict(
    layout: &Layout,
    program: &str,
    version: Version,
    triple: &str,
    digest: &AuthenticatedDigest,
    record: bool,
) -> io::Result<DigestSeen> {
    if !is_vendor(program) || !triple_ok(triple) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "digest log entry is malformed",
        ));
    }
    layout.ensure_dir(&vendor_dir(layout))?;
    let path = digests_path(layout, program);
    let _lock = durable::lock_for_update(&path)?;
    let mut log = read_log(&path)?;
    let key: Key = (
        program.to_string(),
        version,
        triple.to_string(),
        digest.alg(),
    );
    match log.get(&key) {
        Some(first) if first == digest => return Ok(DigestSeen::Same),
        Some(first) => {
            return Ok(DigestSeen::Conflict {
                first: first.clone(),
            });
        }
        None if !record => return Ok(DigestSeen::New),
        None => {}
    }
    log.insert(key, digest.clone());
    bound(&mut log, program);
    let file = LogFile {
        schema: LOG_SCHEMA,
        seen: log
            .into_iter()
            .map(|((program, version, triple, _), digest)| {
                let (sha256, sha512) = digest.keyed();
                Entry {
                    program,
                    version: version.to_string(),
                    triple,
                    sha256,
                    sha512,
                }
            })
            .collect(),
    };
    durable::write_durable(&path, &durable::to_toml(&file, MAX_LOG_BYTES)?)?;
    Ok(DigestSeen::New)
}

/// The log at `path` as a map, empty when there is none yet; entries that do not
/// re-validate are dropped alone.
///
/// # Errors
/// A log that exists but does not read (malformed, oversized, not a regular file, another
/// schema): read as empty, its first sightings would be overwritten unseen.
fn read_log(path: &Path) -> io::Result<BTreeMap<Key, AuthenticatedDigest>> {
    let unreadable = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} does not read; move it aside to start a new log",
                path.display()
            ),
        )
    };
    let Some(file) = durable::read_toml::<LogFile>(path, MAX_LOG_BYTES) else {
        return match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            _ => Err(unreadable()),
        };
    };
    if file.schema != LOG_SCHEMA {
        return Err(unreadable());
    }
    let mut log = BTreeMap::new();
    for e in file.seen {
        let Some(version) = Version::parse(&e.version) else {
            continue;
        };
        let Some(digest) = AuthenticatedDigest::recorded(&e.sha256, &e.sha512) else {
            continue;
        };
        if is_vendor(&e.program) && triple_ok(&e.triple) {
            log.entry((e.program, version, e.triple, digest.alg()))
                .or_insert(digest);
        }
    }
    for program in super::table::VENDORS.iter().map(|s| s.program) {
        bound(&mut log, program);
    }
    Ok(log)
}

fn bound(log: &mut BTreeMap<Key, AuthenticatedDigest>, program: &str) {
    let mine: Vec<Key> = log.keys().filter(|k| k.0 == program).cloned().collect();
    let excess = mine.len().saturating_sub(KEEP_PER_PROGRAM);
    for key in mine.into_iter().take(excess) {
        log.remove(&key);
    }
}

fn triple_ok(t: &str) -> bool {
    let words = t.split('-').count();
    t.len() <= 64
        && (2..=5).contains(&words)
        && t.split('-').all(|w| {
            !w.is_empty()
                && w.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
        })
}

#[cfg(test)]
#[path = "digests_tests.rs"]
mod tests;
