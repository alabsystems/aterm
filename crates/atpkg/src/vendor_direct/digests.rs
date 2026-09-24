// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `<prefix>/vendor/digests.toml`: the first AUTHENTICATED sha256 this machine saw for
//! each `(program, version, triple)`. A later, different digest for the same key is
//! refused — a vendor re-cutting a published version is a supply-chain signal, never an
//! update. Only an [`AuthenticatedDigest`] enters the log: a digest recorded before its
//! anchor vouched for it would let one bad response refuse the genuine one for good.
//! Bounded to the newest [`KEEP_PER_PROGRAM`] versions per program. A log that exists
//! but does not read is refused, never overwritten: its first sightings are the point.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::durable;
use super::stamp::vendor_dir;
use super::table::is_vendor;
use super::version::Version;
use crate::store::Layout;

/// The log's schema.
const LOG_SCHEMA: u32 = 1;

/// Entries kept per program: the highest versions win.
pub(crate) const KEEP_PER_PROGRAM: usize = 64;

/// Bound on a log read.
const MAX_LOG_BYTES: usize = 128 * 1024;

/// A payload sha256 the program's anchor has vouched for: claude's from the manifest
/// whose signature verified, codex's once the release JSON and the release's `SHA256SUMS`
/// agree. Construct one only at that point; [`record_or_conflict`] takes nothing else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuthenticatedDigest(String);

impl AuthenticatedDigest {
    /// `sha256` (64 hex digits, either case), which the anchor has just authenticated.
    #[must_use]
    pub(crate) fn authenticated(sha256: &str) -> Option<Self> {
        let sha256 = sha256.to_ascii_lowercase();
        super::is_hex64(&sha256).then_some(Self(sha256))
    }

    /// The digest, lowercase hex.
    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// What [`record_or_conflict`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DigestSeen {
    /// First sighting; recorded.
    New,
    /// The same digest was seen before.
    Same,
    /// A different digest was seen first for this key; the log keeps it.
    Conflict {
        /// The first-seen sha256.
        first: String,
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
    sha256: String,
}

/// `(program, version, triple)`.
type Key = (String, Version, String);

/// `<prefix>/vendor/digests.toml`.
#[must_use]
pub(crate) fn digests_path(layout: &Layout) -> PathBuf {
    vendor_dir(layout).join("digests.toml")
}

/// Check `digest` against the first digest seen for `(program, version, triple)`, and
/// record it if none was. Only [`DigestSeen::New`] writes; the read and the write hold the
/// log's lock.
///
/// # Errors
/// An argument is malformed (not a vendor program, or a triple that is not `[a-z0-9_.]`
/// words joined by `-`), the log exists but does not read, or it could not be locked or
/// written.
pub(crate) fn record_or_conflict(
    layout: &Layout,
    program: &str,
    version: Version,
    triple: &str,
    digest: &AuthenticatedDigest,
) -> io::Result<DigestSeen> {
    let sha256 = digest.as_str();
    if !is_vendor(program) || !triple_ok(triple) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "digest log entry is malformed",
        ));
    }
    layout.ensure_dir(&vendor_dir(layout))?;
    let path = digests_path(layout);
    let _lock = durable::lock_for_update(&path)?;
    let mut log = read_log(layout)?;
    let key: Key = (program.to_string(), version, triple.to_string());
    match log.get(&key) {
        Some(first) if first == sha256 => return Ok(DigestSeen::Same),
        Some(first) => {
            return Ok(DigestSeen::Conflict {
                first: first.clone(),
            });
        }
        None => {}
    }
    log.insert(key, sha256.to_string());
    bound(&mut log, program);
    let file = LogFile {
        schema: LOG_SCHEMA,
        seen: log
            .into_iter()
            .map(|((program, version, triple), sha256)| Entry {
                program,
                version: version.to_string(),
                triple,
                sha256,
            })
            .collect(),
    };
    durable::write_durable(&path, &durable::to_toml(&file, MAX_LOG_BYTES)?)?;
    Ok(DigestSeen::New)
}

/// The log as a map — empty when there is none yet; entries that do not re-validate are
/// dropped alone.
///
/// # Errors
/// A log that exists but does not read (malformed, oversized, not a regular file, another
/// schema): treated as empty, its first sightings would be overwritten unseen.
fn read_log(layout: &Layout) -> io::Result<BTreeMap<Key, String>> {
    let path = digests_path(layout);
    let unreadable = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} does not read; move it aside to start a new log",
                path.display()
            ),
        )
    };
    let Some(file) = durable::read_toml::<LogFile>(&path, MAX_LOG_BYTES) else {
        return match std::fs::symlink_metadata(&path) {
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
        if is_vendor(&e.program) && triple_ok(&e.triple) && super::is_hex64(&e.sha256) {
            log.entry((e.program, version, e.triple))
                .or_insert(e.sha256);
        }
    }
    for program in super::table::VENDORS.iter().map(|s| s.program) {
        bound(&mut log, program);
    }
    Ok(log)
}

/// Drop `program`'s lowest versions until [`KEEP_PER_PROGRAM`] remain.
fn bound(log: &mut BTreeMap<Key, String>, program: &str) {
    let mine: Vec<Key> = log.keys().filter(|k| k.0 == program).cloned().collect();
    let excess = mine.len().saturating_sub(KEEP_PER_PROGRAM);
    // Keys sort by (program, version, triple), so the first are the lowest versions.
    for key in mine.into_iter().take(excess) {
        log.remove(&key);
    }
}

/// `aarch64-apple-darwin`-shaped: 2–5 `-`-joined words of `[a-z0-9_.]`, at most 64 bytes.
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
mod tests {
    use super::*;

    const T: &str = "aarch64-apple-darwin";

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!(
            "atpkg-vendor-digests-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        Layout { prefix: p }
    }

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn sha(c: char) -> String {
        c.to_string().repeat(64)
    }

    /// [`record_or_conflict`] of a digest the test vouches for.
    fn rec(
        l: &Layout,
        program: &str,
        version: Version,
        triple: &str,
        sha256: &str,
    ) -> io::Result<DigestSeen> {
        let digest = AuthenticatedDigest::authenticated(sha256).unwrap();
        record_or_conflict(l, program, version, triple, &digest)
    }

    #[test]
    fn first_seen_wins_and_a_different_digest_conflicts() {
        let l = layout("first");
        let seen = |p, ver, t, s: &str| rec(&l, p, v(ver), t, s).unwrap();
        assert_eq!(seen("claude", "2.1.280", T, &sha('a')), DigestSeen::New);
        assert_eq!(seen("claude", "2.1.280", T, &sha('a')), DigestSeen::Same);
        assert_eq!(
            seen("claude", "2.1.280", T, &sha('A')),
            DigestSeen::Same,
            "hex case is not a different digest"
        );
        assert_eq!(
            seen("claude", "2.1.280", T, &sha('b')),
            DigestSeen::Conflict { first: sha('a') }
        );
        // The conflict did not replace the first sighting.
        assert_eq!(
            seen("claude", "2.1.280", T, &sha('b')),
            DigestSeen::Conflict { first: sha('a') }
        );
        // Other triples, versions and programs are other keys.
        assert_eq!(
            seen("claude", "2.1.280", "x86_64-apple-darwin", &sha('b')),
            DigestSeen::New
        );
        assert_eq!(seen("claude", "2.1.281", T, &sha('b')), DigestSeen::New);
        assert_eq!(seen("codex", "2.1.280", T, &sha('b')), DigestSeen::New);
    }

    #[test]
    fn malformed_arguments_are_refused_and_write_nothing() {
        let l = layout("args");
        for (p, t) in [
            ("trust", T),
            ("claude", "darwin"),
            ("claude", "../x-y"),
            ("claude", "Aarch64-apple-darwin"),
        ] {
            assert!(rec(&l, p, v("1.0.0"), t, &sha('a')).is_err(), "{p} {t}");
        }
        assert!(!digests_path(&l).exists());
    }

    #[test]
    fn only_a_sha256_is_an_authenticated_digest() {
        for bad in [
            String::from("abc"),
            sha('g'),
            String::new(),
            format!("{}0", sha('a')),
        ] {
            assert_eq!(AuthenticatedDigest::authenticated(&bad), None, "{bad}");
        }
        let d = AuthenticatedDigest::authenticated(&sha('A')).unwrap();
        assert_eq!(d.as_str(), sha('a'), "held lowercase");
    }

    #[test]
    fn the_log_keeps_the_newest_versions_per_program() {
        let l = layout("bound");
        let total = KEEP_PER_PROGRAM + 6;
        for patch in 0..total {
            let ver = Version::new(1, 0, u32::try_from(patch).unwrap()).unwrap();
            rec(&l, "codex", ver, T, &sha('a')).unwrap();
        }
        rec(&l, "claude", v("1.0.0"), T, &sha('c')).unwrap();
        let log = read_log(&l).unwrap();
        let codex: Vec<Version> = log.keys().filter(|k| k.0 == "codex").map(|k| k.1).collect();
        assert_eq!(codex.len(), KEEP_PER_PROGRAM);
        assert_eq!(codex[0], v("1.0.6"), "the six lowest were dropped");
        assert!(
            log.keys().any(|k| k.0 == "claude"),
            "one program's bound never evicts another's"
        );
        // An evicted version records afresh rather than conflicting.
        assert_eq!(
            rec(&l, "codex", v("1.0.0"), T, &sha('b')).unwrap(),
            DigestSeen::New
        );
    }

    /// A log that does not read is refused and left as it is — never overwritten with a
    /// fresh one that would accept a re-cut as a first sighting; one bad entry drops alone.
    #[test]
    fn an_unreadable_log_is_refused_and_a_bad_entry_drops_alone() {
        let l = layout("malformed");
        rec(&l, "claude", v("2.1.280"), T, &sha('a')).unwrap();
        let path = digests_path(&l);
        std::fs::write(&path, "not toml {{{").unwrap();
        assert!(read_log(&l).is_err());
        let err = rec(&l, "claude", v("2.1.280"), T, &sha('b')).unwrap_err();
        assert!(err.to_string().contains("does not read"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "not toml {{{",
            "the unreadable log is left for a person to look at"
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            rec(&l, "claude", v("2.1.280"), T, &sha('b')).unwrap(),
            DigestSeen::New,
            "no log at all starts a new one"
        );
        std::fs::write(
            &path,
            format!(
                "schema = 1\n\
                 [[seen]]\nprogram = \"claude\"\nversion = \"2.1.08\"\ntriple = \"{T}\"\nsha256 = \"{a}\"\n\
                 [[seen]]\nprogram = \"claude\"\nversion = \"2.1.281\"\ntriple = \"{T}\"\nsha256 = \"{a}\"\n",
                a = sha('a')
            ),
        )
        .unwrap();
        let log = read_log(&l).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(
            rec(&l, "claude", v("2.1.281"), T, &sha('b')).unwrap(),
            DigestSeen::Conflict { first: sha('a') }
        );
        std::fs::write(&path, "schema = 9\n").unwrap();
        assert!(read_log(&l).is_err(), "an unknown schema is not read");
        assert!(
            rec(&l, "claude", v("2.1.282"), T, &sha('a')).is_err(),
            "…nor overwritten"
        );
    }
}
