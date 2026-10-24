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

    /// `hex` (one `alg` digest, either case), which the lane measured from the bytes it
    /// downloaded for an anchor no document of which names a digest (grok): what vouches for
    /// those bytes is the darwin Developer ID team pin at stage, not a document.
    #[must_use]
    pub(crate) fn measured(alg: DigestAlg, hex: &str) -> Option<Self> {
        Self::authenticated(alg, hex)
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

type Key = (String, Version, String);

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

/// Check `digest` against the first digest seen for `(program, version, triple)` and, when
/// `record`, record it if none was.
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
    let key: Key = (program.to_string(), version, triple.to_string());
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
            .map(|((program, version, triple), digest)| {
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
            log.entry((e.program, version, e.triple)).or_insert(digest);
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
mod tests {
    use super::*;

    const T: &str = "aarch64-apple-darwin";

    fn layout(label: &str) -> crate::testdir::ScratchLayout {
        crate::testdir::layout(&format!("atpkg-vendor-digests-{label}-"))
    }

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn sha(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn d256(hex: &str) -> AuthenticatedDigest {
        AuthenticatedDigest::authenticated(DigestAlg::Sha256, hex).unwrap()
    }

    fn d512(c: char) -> AuthenticatedDigest {
        AuthenticatedDigest::authenticated(DigestAlg::Sha512, &c.to_string().repeat(128)).unwrap()
    }

    fn rec(
        l: &Layout,
        program: &str,
        version: Version,
        triple: &str,
        sha256: &str,
    ) -> io::Result<DigestSeen> {
        record_or_conflict(l, program, version, triple, &d256(sha256), true)
    }

    fn rec512(l: &Layout, program: &str, version: &str, c: char) -> DigestSeen {
        record_or_conflict(l, program, v(version), T, &d512(c), true).unwrap()
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
            DigestSeen::Conflict {
                first: d256(&sha('a'))
            }
        );
        assert_eq!(
            seen("claude", "2.1.280", T, &sha('b')),
            DigestSeen::Conflict {
                first: d256(&sha('a'))
            }
        );
        assert_eq!(
            seen("claude", "2.1.280", "x86_64-apple-darwin", &sha('b')),
            DigestSeen::New
        );
        assert_eq!(seen("claude", "2.1.281", T, &sha('b')), DigestSeen::New);
        assert_eq!(seen("codex", "2.1.280", T, &sha('b')), DigestSeen::New);
        let check =
            |ver, s: &str| record_or_conflict(&l, "claude", v(ver), T, &d256(s), false).unwrap();
        assert_eq!(check("2.1.290", &sha('a')), DigestSeen::New);
        assert_eq!(seen("claude", "2.1.290", T, &sha('b')), DigestSeen::New);
        assert_eq!(
            check("2.1.280", &sha('b')),
            DigestSeen::Conflict {
                first: d256(&sha('a'))
            }
        );
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
        assert!(!vendor_dir(&l).exists());
    }

    #[test]
    fn only_a_digest_of_its_algorithm_is_an_authenticated_digest() {
        const AGY_SHA512: &str = "473a34b685d0a09d48790e8b86bc3e545dfab889fe0d8fc81cb7e1fdfc336fe7\
                                  8ce57022e3b438c754c93c9cffeb13fdbd177e50fdedc087d52732fa5475183f";
        for (alg, bad) in [
            (DigestAlg::Sha256, String::from("abc")),
            (DigestAlg::Sha256, sha('g')),
            (DigestAlg::Sha256, String::new()),
            (DigestAlg::Sha256, format!("{}0", sha('a'))),
            (DigestAlg::Sha256, AGY_SHA512.to_string()),
            (DigestAlg::Sha512, sha('a')),
            (DigestAlg::Sha512, format!("{AGY_SHA512}0")),
        ] {
            assert_eq!(
                AuthenticatedDigest::authenticated(alg, &bad),
                None,
                "{alg} {bad}"
            );
        }
        let d = d256(&sha('A'));
        assert_eq!(
            (d.alg(), d.as_str()),
            (DigestAlg::Sha256, sha('a').as_str()),
            "held lowercase"
        );
        let d = AuthenticatedDigest::authenticated(DigestAlg::Sha512, &AGY_SHA512.to_uppercase())
            .unwrap();
        assert_eq!((d.alg(), d.as_str()), (DigestAlg::Sha512, AGY_SHA512));
        assert_eq!(d.keyed(), (String::new(), AGY_SHA512.to_string()));
        assert_eq!(AuthenticatedDigest::recorded("", AGY_SHA512), Some(d));
        for (sha256, sha512) in [
            (sha('a'), AGY_SHA512.to_string()),
            (String::new(), String::new()),
            (sha('A'), String::new()),
            (String::new(), AGY_SHA512.to_uppercase()),
            (AGY_SHA512.to_string(), String::new()),
        ] {
            assert_eq!(
                AuthenticatedDigest::recorded(&sha256, &sha512),
                None,
                "{sha256} {sha512}"
            );
        }
    }

    #[test]
    fn each_digest_is_logged_under_its_algorithm() {
        let l = layout("alg");
        let shared = digests_path(&l, "claude");
        std::fs::create_dir_all(shared.parent().unwrap()).unwrap();
        let earlier = format!(
            "[[seen]]\nprogram = \"claude\"\nversion = \"2.1.280\"\ntriple = \"{T}\"\nsha256 = \"{a}\"\n",
            a = sha('a')
        );
        std::fs::write(&shared, format!("schema = 1\n\n{earlier}")).unwrap();
        assert_eq!(rec512(&l, "agy", "1.3.6", '5'), DigestSeen::New);
        assert_eq!(
            rec(&l, "codex", v("0.157.0"), T, &sha('b')).unwrap(),
            DigestSeen::New
        );
        let text = std::fs::read_to_string(&shared).unwrap();
        assert!(
            text.contains(&earlier),
            "the earlier entry is rewritten as it was: {text}"
        );
        #[derive(Deserialize)]
        struct EarlierEntry {
            program: String,
            version: String,
            triple: String,
            sha256: String,
        }
        #[derive(Deserialize)]
        struct EarlierLog {
            seen: Vec<EarlierEntry>,
        }
        let earlier_reads: EarlierLog = aterm_toml::from_str(&text).unwrap();
        let rows: Vec<[String; 4]> = earlier_reads
            .seen
            .into_iter()
            .map(|e| [e.program, e.version, e.triple, e.sha256])
            .collect();
        assert_eq!(
            rows,
            [
                ["claude", "2.1.280", T, &sha('a')].map(String::from),
                ["codex", "0.157.0", T, &sha('b')].map(String::from),
            ],
            "{text}"
        );
        let agy = digests_path(&l, "agy");
        let text = std::fs::read_to_string(&agy).unwrap();
        assert!(
            text.contains(&format!("sha512 = \"{}\"\n", "5".repeat(128)))
                && !text.contains("sha256"),
            "{text}"
        );
        assert_eq!(rec512(&l, "agy", "1.3.6", '5'), DigestSeen::Same);
        assert_eq!(
            rec512(&l, "agy", "1.3.6", '6'),
            DigestSeen::Conflict { first: d512('5') }
        );
        assert_eq!(
            rec(&l, "agy", v("1.3.6"), T, &sha('5')).unwrap(),
            DigestSeen::Conflict { first: d512('5') },
            "a sha256 for a key first seen with a sha512 is not the same digest"
        );
        let both = format!(
            "[[seen]]\nprogram = \"agy\"\nversion = \"1.3.7\"\ntriple = \"{T}\"\n\
             sha256 = \"{a}\"\nsha512 = \"{b}\"\n\
             [[seen]]\nprogram = \"agy\"\nversion = \"1.3.8\"\ntriple = \"{T}\"\n",
            a = sha('a'),
            b = "b".repeat(128)
        );
        std::fs::write(&agy, format!("{text}{both}")).unwrap();
        assert_eq!(
            read_log(&agy).unwrap().len(),
            1,
            "the two-digest and no-digest entries dropped alone"
        );
        assert_eq!(
            rec(&l, "agy", v("1.3.7"), T, &sha('c')).unwrap(),
            DigestSeen::New
        );
    }

    #[test]
    fn a_later_programs_sightings_outlive_an_older_clients_rewrite() {
        let l = layout("older");
        assert_eq!(rec512(&l, "agy", "1.3.6", '5'), DigestSeen::New);
        let muse = || v("1.4.9-R6297.1");
        assert_eq!(
            rec(&l, "muse", muse(), T, &sha('a')).unwrap(),
            DigestSeen::New
        );
        assert_eq!(
            rec(&l, "codex", v("0.157.0"), T, &sha('a')).unwrap(),
            DigestSeen::New
        );
        std::fs::write(
            digests_path(&l, "codex"),
            format!(
                "schema = 1\n[[seen]]\nprogram = \"codex\"\nversion = \"0.163.0\"\n\
                 triple = \"{T}\"\nsha256 = \"{b}\"\n",
                b = sha('b')
            ),
        )
        .unwrap();
        assert_eq!(
            rec512(&l, "agy", "1.3.6", '6'),
            DigestSeen::Conflict { first: d512('5') }
        );
        assert_eq!(
            rec(&l, "muse", muse(), T, &sha('b')).unwrap(),
            DigestSeen::Conflict {
                first: d256(&sha('a'))
            }
        );
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
        let log = read_log(&digests_path(&l, "codex")).unwrap();
        let codex: Vec<Version> = log.keys().filter(|k| k.0 == "codex").map(|k| k.1).collect();
        assert_eq!(codex.len(), KEEP_PER_PROGRAM);
        assert_eq!(codex[0], v("1.0.6"), "the six lowest were dropped");
        assert!(
            log.keys().any(|k| k.0 == "claude"),
            "one program's bound never evicts another's"
        );
        assert_eq!(
            rec(&l, "codex", v("1.0.0"), T, &sha('b')).unwrap(),
            DigestSeen::New
        );
    }

    #[test]
    fn an_unreadable_log_is_refused_and_a_bad_entry_drops_alone() {
        let l = layout("malformed");
        rec(&l, "claude", v("2.1.280"), T, &sha('a')).unwrap();
        let path = digests_path(&l, "claude");
        std::fs::write(&path, "not toml {{{").unwrap();
        assert!(read_log(&path).is_err());
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
        let log = read_log(&path).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(
            rec(&l, "claude", v("2.1.281"), T, &sha('b')).unwrap(),
            DigestSeen::Conflict {
                first: d256(&sha('a'))
            }
        );
        std::fs::write(&path, "schema = 9\n").unwrap();
        assert!(read_log(&path).is_err(), "an unknown schema is not read");
        assert!(
            rec(&l, "claude", v("2.1.282"), T, &sha('a')).is_err(),
            "…nor overwritten"
        );
    }
}
