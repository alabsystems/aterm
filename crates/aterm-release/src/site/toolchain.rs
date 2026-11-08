// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Mac architecture coverage of the pinned ALab toolset in the atpkg index.

use std::collections::BTreeMap;
use std::fmt;

use aterm_toml::Value;
use atpkg::signed_artifact_cache::{SignedArtifactCache, SignedArtifactKey, SignedIndexKey};
use atpkg::{Anchor, BuildFloor, Candidate, Layout, TrustedIndex};

use super::releases::Release;

const DOCUMENT_LIMIT: u64 = atpkg::net::MANIFEST_CAP;
const SIGNATURE_LIMIT: u64 = atpkg::net::SIG_CAP;
const ROSTER_LIMIT: u64 = atpkg::net::ROSTER_CAP;

fn memory_limit(limit: u64) -> usize {
    usize::try_from(limit).unwrap_or(usize::MAX)
}

/// The Mac architectures the page carries a level for, in attribute order.
pub const MAC_ARCHES: [&str; 2] = ["x86_64", "aarch64"];

/// How much of the pinned set serves one Mac architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// No pinned program carries the architecture.
    Absent,
    Partial,
    /// Every pinned program carries it.
    Full,
}

impl Coverage {
    /// The attribute value the site's `platform.js` reads.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "none",
            Self::Partial => "partial",
            Self::Full => "full",
        }
    }
}

/// Per [`MAC_ARCHES`] entry, its coverage.
pub type Levels = [Coverage; 2];

/// Why the levels could not be computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unknown {
    /// The release stream holds no `atpkg-index-<n>` release with an `index.toml`.
    NoIndex { slug: String },
    /// A manifest the stable channel pins was not there to read.
    Missing { manifest: String, triple: String },
    /// Anything else the index or a manifest said wrong, or a failed read.
    Other(String),
}

impl fmt::Display for Unknown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoIndex { slug } => {
                write!(f, "no atpkg-index-<n> release on {slug} carries index.toml")
            }
            Self::Missing { manifest, triple } => write!(
                f,
                "{manifest} is missing — cannot tell whether it serves {triple}"
            ),
            Self::Other(why) => f.write_str(why),
        }
    }
}

fn triple(arch: &str) -> String {
    format!("{arch}-apple-darwin")
}

/// `pkg-<program>-<build>.toml`.
#[must_use]
pub fn manifest_name(program: &str, build: u64) -> String {
    format!("pkg-{program}-{build}.toml")
}

fn parse(text: &str, what: &str) -> Result<Value, Unknown> {
    aterm_toml::from_str(text).map_err(|e| Unknown::Other(format!("{what} is not TOML: {e}")))
}

/// The stable channel's `{program: build}` pins per Mac architecture.
///
/// # Errors
/// Not exactly one stable channel, or a pin that is not a program name and a build.
pub fn mac_pins(index: &Value) -> Result<[BTreeMap<String, u64>; 2], Unknown> {
    let empty = Vec::new();
    let channels: Vec<&Value> = match index.get("channels") {
        Some(Value::Array(list)) => list,
        _ => &empty,
    }
    .iter()
    .filter(|c| c.get("name").and_then(Value::as_str) == Some("stable"))
    .collect();
    let [channel] = channels.as_slice() else {
        return Err(Unknown::Other(format!(
            "index: expected exactly one [[channels]] named stable, found {}",
            channels.len()
        )));
    };
    let programs = index.get("programs").and_then(Value::as_table);
    let base_pin = channel.get("pin").and_then(Value::as_table);
    let by_target = channel.get("pin_by_target").and_then(Value::as_table);
    let mut out: [BTreeMap<String, u64>; 2] = Default::default();
    for (slot, arch) in MAC_ARCHES.iter().enumerate() {
        let mut view: BTreeMap<&str, &Value> = base_pin
            .into_iter()
            .flatten()
            .map(|(k, v)| (k.as_str(), v))
            .collect();
        if let Some(overlay) = by_target
            .and_then(|t| t.get(&triple(arch)))
            .and_then(Value::as_table)
        {
            for (k, v) in overlay {
                view.insert(k.as_str(), v);
            }
        }
        for (program, build) in view {
            if programs.and_then(|p| p.get(program)).is_none() {
                continue;
            }
            let name_ok = !program.is_empty()
                && program
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
            match build {
                Value::Integer(n) if name_ok && *n >= 0 => {
                    out[slot].insert(program.to_string(), n.unsigned_abs());
                }
                other => {
                    return Err(Unknown::Other(format!(
                        "index: stable pin {program:?} = {other:?} is not a program name and a \
                         build number"
                    )));
                }
            }
        }
    }
    Ok(out)
}

/// The coverage of each Mac architecture over the stable pins.
///
/// # Errors
/// If [`mac_pins`] refuses the index, an arch has no pins, or a pinned manifest is missing or
/// invalid.
pub fn levels(index: &Value, manifest: &dyn Fn(&str) -> Option<String>) -> Result<Levels, Unknown> {
    let mut levels = [Coverage::Absent; 2];
    let mut parsed_manifests: BTreeMap<String, Value> = BTreeMap::new();
    for (slot, pins) in mac_pins(index)?.iter().enumerate() {
        let triple = triple(MAC_ARCHES[slot]);
        if pins.is_empty() {
            return Err(Unknown::Other(format!(
                "index: the stable channel pins no program for {triple}"
            )));
        }
        let mut serving = 0;
        for (program, build) in pins {
            let name = manifest_name(program, *build);
            let m = if let Some(cached) = parsed_manifests.get(&name) {
                cached
            } else {
                let text = manifest(&name).ok_or_else(|| Unknown::Missing {
                    manifest: name.clone(),
                    triple: triple.clone(),
                })?;
                let m = parse(&text, &name)?;
                parsed_manifests.insert(name.clone(), m);
                &parsed_manifests[&name]
            };
            let names = m.get("program").and_then(Value::as_str) == Some(program.as_str());
            let builds = matches!(m.get("build_number"), Some(Value::Integer(n)) if *n >= 0 && n.unsigned_abs() == *build);
            if !names || !builds {
                return Err(Unknown::Other(format!(
                    "toolchain: {name} names {:?} build {:?}",
                    m.get("program"),
                    m.get("build_number")
                )));
            }
            let carries = match m.get("artifact") {
                Some(Value::Array(rows)) => rows
                    .iter()
                    .any(|r| r.get("target").and_then(Value::as_str) == Some(triple.as_str())),
                _ => false,
            };
            serving += usize::from(carries);
        }
        levels[slot] = match serving {
            0 => Coverage::Absent,
            n if n == pins.len() => Coverage::Full,
            _ => Coverage::Partial,
        };
    }
    Ok(levels)
}

/// The newest non-draft `atpkg-index-<n>` release that carries the complete signed-index quad.
#[must_use]
pub fn index_release(releases: &[Release]) -> Option<&Release> {
    let n = |r: &Release| -> Option<u64> {
        let digits = r.tag.strip_prefix("atpkg-index-")?;
        let canonical = !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit())
            && (digits == "0" || !digits.starts_with('0'));
        canonical.then(|| digits.parse().ok()).flatten()
    };
    releases
        .iter()
        .filter(|r| {
            !r.draft
                && [
                    "index.toml",
                    "index.toml.sig",
                    "aterm-machines.toml",
                    "aterm-machines.toml.sig",
                ]
                .iter()
                .all(|name| r.named(name).is_some())
        })
        .filter_map(|r| n(r).map(|k| (k, r)))
        .max_by_key(|(k, _)| *k)
        .map(|(_, r)| r)
}

/// What [`fetch`] read.
#[derive(Debug)]
pub struct Fetched {
    pub tag: String,
    pub index: String,
    pub manifests: BTreeMap<String, String>,
}

fn index_build(tag: &str) -> Option<u64> {
    let digits = tag.strip_prefix("atpkg-index-")?;
    let canonical = !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && (digits == "0" || !digits.starts_with('0'));
    canonical.then(|| digits.parse().ok()).flatten()
}

fn verify_index_candidate(
    candidate: Candidate,
    masters: &[&str],
    expected_build: u64,
    now_unix: i64,
) -> Result<TrustedIndex, Unknown> {
    let label = candidate.label.clone();
    let selected = atpkg::select_index(
        &Anchor::of(masters.iter().map(|key| (*key).to_string()).collect(), 0),
        vec![candidate],
        BuildFloor::none(),
        None,
        now_unix,
    )
    .selected
    .ok_or_else(|| Unknown::Other(format!("{label}: signed index chain refused")))?;
    if selected.label != label || selected.index.index_build != expected_build {
        return Err(Unknown::Other(format!(
            "{label}: release names index build {expected_build}, signed index names {}",
            selected.index.index_build
        )));
    }
    let until = aterm_types::rfc3339::parse_utc_fractional(&selected.index.valid_until)
        .ok_or_else(|| Unknown::Other(format!("{label}: invalid index freshness deadline")))?;
    atpkg::check_freshness(now_unix, until)
        .map_err(|_| Unknown::Other(format!("{label}: signed index is stale")))?;
    Ok(selected.index)
}

fn verify_package(
    index: &TrustedIndex,
    raw: Vec<u8>,
    signature: &[u8],
    program: &str,
    build: u64,
) -> Result<(atpkg::PkgManifest, Vec<u8>), Unknown> {
    let verified = index.verify_pkg(raw, signature).map_err(|why| {
        Unknown::Other(format!(
            "{}: package signature refused ({why:?})",
            manifest_name(program, build)
        ))
    })?;
    let manifest = atpkg::parse_pkg(&verified).map_err(|why| {
        Unknown::Other(format!(
            "{}: verified package manifest refused ({why:?})",
            manifest_name(program, build)
        ))
    })?;
    if manifest.program != program || manifest.build_number != build {
        return Err(Unknown::Other(format!(
            "{}: signed manifest names {} build {}",
            manifest_name(program, build),
            manifest.program,
            manifest.build_number
        )));
    }
    Ok((manifest, verified.as_slice().to_vec()))
}

/// The newest index and every manifest its stable channel pins for a Mac.
///
/// # Errors
/// No complete signed index release, a failed read, a signature or identity refusal, an index
/// that [`mac_pins`] refuses, or one that names no repository for a pinned program.
pub fn fetch(
    releases: &[Release],
    slug: &str,
    base: &str,
    layout: Option<&Layout>,
    masters: &[&str],
    now_unix: i64,
    get: &mut super::Fetch<'_>,
) -> Result<Fetched, Unknown> {
    let index_release =
        index_release(releases).ok_or_else(|| Unknown::NoIndex { slug: slug.into() })?;
    let (owner, repo) = slug.split_once('/').unwrap_or((slug, ""));
    if repo != atpkg::manifest::INDEX_REPO {
        return Err(Unknown::Other(format!(
            "index source {slug} is not the canonical {owner}/{} repository",
            atpkg::manifest::INDEX_REPO
        )));
    }
    let expected_build = index_build(&index_release.tag)
        .ok_or_else(|| Unknown::Other(format!("invalid index tag {}", index_release.tag)))?;
    let base = base.trim_end_matches('/');
    let mut read = |repo: &str, tag: &str, name: &str, limit: u64| -> Result<Vec<u8>, Unknown> {
        let url = format!("{base}/{owner}/{repo}/releases/download/{tag}/{name}");
        get(&url, limit).map_err(|e| Unknown::Other(format!("fetch {url}: {e}")))
    };
    let source = atpkg::net::github_source_id(owner);
    let index_key = SignedIndexKey::release(&source, &index_release.tag)
        .ok_or_else(|| Unknown::Other(format!("invalid index tag {}", index_release.tag)))?;
    let cached_candidate = layout.and_then(|layout| {
        SignedArtifactCache::for_layout(layout).load_index(
            &index_key,
            memory_limit(DOCUMENT_LIMIT),
            memory_limit(SIGNATURE_LIMIT),
            memory_limit(ROSTER_LIMIT),
            memory_limit(SIGNATURE_LIMIT),
        )
    });
    let cached_index = cached_candidate.as_ref().and_then(|candidate| {
        verify_index_candidate(candidate.clone(), masters, expected_build, now_unix).ok()
    });
    let (candidate, index) = if let (Some(candidate), Some(index)) =
        (cached_candidate, cached_index)
    {
        (candidate, index)
    } else {
        let candidate = Candidate {
            label: index_release.tag.clone(),
            index_bytes: read(repo, &index_release.tag, "index.toml", DOCUMENT_LIMIT)?,
            sig: read(repo, &index_release.tag, "index.toml.sig", SIGNATURE_LIMIT)?,
            roster_bytes: read(
                repo,
                &index_release.tag,
                "aterm-machines.toml",
                ROSTER_LIMIT,
            )?,
            roster_sig: read(
                repo,
                &index_release.tag,
                "aterm-machines.toml.sig",
                SIGNATURE_LIMIT,
            )?,
        };
        let index = verify_index_candidate(candidate.clone(), masters, expected_build, now_unix)?;
        if let Some(layout) = layout {
            let _ = SignedArtifactCache::for_layout(layout).store_verified_index(
                &index_key,
                &candidate,
                memory_limit(DOCUMENT_LIMIT),
                memory_limit(SIGNATURE_LIMIT),
                memory_limit(ROSTER_LIMIT),
                memory_limit(SIGNATURE_LIMIT),
            );
        }
        (candidate, index)
    };
    let index_text = String::from_utf8(candidate.index_bytes)
        .map_err(|_| Unknown::Other("verified index.toml is not UTF-8".into()))?;
    let index_value = parse(&index_text, "index.toml")?;
    let mut wanted = std::collections::BTreeSet::new();
    for pins in mac_pins(&index_value)? {
        wanted.extend(pins);
    }
    let mut manifests = BTreeMap::new();
    for (program, build) in wanted {
        let repo = index
            .program(&program)
            .map(|p| p.repo.as_str())
            .filter(|r| {
                !r.is_empty()
                    && r.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
            })
            .ok_or_else(|| {
                Unknown::Other(format!("index: program {program} names no repository"))
            })?;
        let name = manifest_name(&program, build);
        let tag = format!("atpkg-{program}-{build}");
        let package_key = SignedArtifactKey::package(&source, repo, &program, build);
        let cached = layout.and_then(|layout| {
            SignedArtifactCache::for_layout(layout).load(
                &package_key,
                memory_limit(DOCUMENT_LIMIT),
                memory_limit(SIGNATURE_LIMIT),
            )
        });
        let cached_verified = cached.as_ref().and_then(|(raw, signature)| {
            verify_package(&index, raw.clone(), signature, &program, build).ok()
        });
        let (_manifest, raw) = if let Some(verified) = cached_verified {
            verified
        } else {
            let raw = read(repo, &tag, &name, DOCUMENT_LIMIT)?;
            let signature = read(repo, &tag, &format!("{name}.sig"), SIGNATURE_LIMIT)?;
            let verified = verify_package(&index, raw, &signature, &program, build)?;
            if let Some(layout) = layout {
                let _ = SignedArtifactCache::for_layout(layout).store_verified(
                    &package_key,
                    &verified.1,
                    &signature,
                    memory_limit(DOCUMENT_LIMIT),
                    memory_limit(SIGNATURE_LIMIT),
                );
            }
            verified
        };
        let text = String::from_utf8(raw)
            .map_err(|_| Unknown::Other(format!("{name}: verified bytes are not UTF-8")))?;
        manifests.insert(name, text);
    }
    Ok(Fetched {
        tag: index_release.tag.clone(),
        index: index_text,
        manifests,
    })
}

/// [`levels`] over what [`fetch`] read.
///
/// # Errors
/// As [`levels`].
pub fn fetched_levels(f: &Fetched) -> Result<Levels, Unknown> {
    levels(&parse(&f.index, "index.toml")?, &|name| {
        f.manifests.get(name).cloned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::releases::parse_releases;
    use ring::signature::{Ed25519KeyPair, KeyPair as _};
    use std::cell::Cell;
    use std::path::{Path, PathBuf};

    const NOW: i64 = 1_785_801_600;
    const MASTER: [u8; 32] = [0x71; 32];
    const MACHINE: [u8; 32] = [0x72; 32];

    fn fixture() -> PathBuf {
        aterm_tempfile::manifest_dir().join("tests/fixtures/site/toolchain")
    }

    fn read_dir(dir: &Path) -> BTreeMap<String, String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .map(|p| {
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                (name, std::fs::read_to_string(&p).unwrap())
            })
            .collect()
    }

    fn levels_of(files: &BTreeMap<String, String>) -> Result<Levels, Unknown> {
        let index = aterm_toml::from_str(&files["index.toml"]).unwrap();
        levels(&index, &|name| files.get(name).cloned())
    }

    fn kp(seed: &[u8; 32]) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(seed).unwrap()
    }

    fn public_key(seed: &[u8; 32]) -> String {
        aterm_codec::base64::encode(kp(seed).public_key().as_ref()).unwrap()
    }

    fn sign(seed: &[u8; 32], bytes: &[u8]) -> Vec<u8> {
        kp(seed).sign(bytes).as_ref().to_vec()
    }

    fn roster() -> Vec<u8> {
        format!(
            "schema = 1\nroster_seq = 3\nvalid_until = \"2099-01-01T00:00:00Z\"\n\
             revoked = []\n\n[[machine]]\nid = \"m3\"\npubkey = \"{}\"\n\
             added_at = \"2026-08-01T00:00:00Z\"\n",
            public_key(&MACHINE)
        )
        .into_bytes()
    }

    fn signed_fixture() -> (
        Vec<crate::site::releases::Release>,
        BTreeMap<String, Vec<u8>>,
    ) {
        let files = read_dir(&fixture());
        let index = files["index.toml"]
            .replace("machine_id = \"m0\"", "machine_id = \"m3\"")
            .replace("roster_seq = 1", "roster_seq = 3")
            .into_bytes();
        let roster = roster();
        let mut served = BTreeMap::new();
        let url = |repo: &str, tag: &str, name: &str| {
            format!("file:///dl/alabsystems/{repo}/releases/download/{tag}/{name}")
        };
        served.insert(
            url("aterm", "atpkg-index-50", "index.toml.sig"),
            sign(&MACHINE, &index),
        );
        served.insert(
            url("aterm", "atpkg-index-50", "aterm-machines.toml.sig"),
            sign(&MASTER, &roster),
        );
        served.insert(
            url("aterm", "atpkg-index-50", "aterm-machines.toml"),
            roster,
        );
        served.insert(url("aterm", "atpkg-index-50", "index.toml"), index);
        for (repo, program, build) in [
            ("ay", "ay", 8256),
            ("aterm", "claude", 2_026_092_601),
            ("trust", "trust", 9192),
        ] {
            let name = manifest_name(program, build);
            let tag = format!("atpkg-{program}-{build}");
            let raw = files[&name]
                .replace(
                    "kind = \"binary\"",
                    "kind = \"binary\"\nasset = \"fixture\"",
                )
                .into_bytes();
            served.insert(
                url(repo, &tag, &format!("{name}.sig")),
                sign(&MACHINE, &raw),
            );
            served.insert(url(repo, &tag, &name), raw);
        }
        let releases = parse_releases(
            r#"[{"tag_name":"atpkg-index-51","prerelease":true,"draft":true,"assets":[{"name":"index.toml","size":1}]},
                {"tag_name":"atpkg-index-50","prerelease":true,"draft":false,"assets":[
                    {"name":"index.toml","size":1},{"name":"index.toml.sig","size":64},
                    {"name":"aterm-machines.toml","size":1},{"name":"aterm-machines.toml.sig","size":64}]},
                {"tag_name":"atpkg-index-49","prerelease":true,"draft":false,"assets":[{"name":"index.toml","size":1}]}]"#,
        )
        .unwrap();
        (releases, served)
    }

    fn cache_layout(root: &aterm_tempfile::TempDir) -> Layout {
        Layout {
            prefix: root.path().join("pkg"),
        }
    }

    fn corrupt_objects(path: &Path) {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    corrupt_objects(&path);
                } else {
                    std::fs::write(path, b"corrupt").unwrap();
                }
            }
        }
    }

    #[test]
    fn the_fixture_index_is_full_on_arm_and_partial_on_intel() {
        let files = read_dir(&fixture());
        assert_eq!(
            levels_of(&files).unwrap(),
            [Coverage::Partial, Coverage::Full]
        );
    }

    #[test]
    fn a_per_target_overlay_is_what_that_triple_sees() {
        let mut files = read_dir(&fixture());
        files
            .get_mut("index.toml")
            .unwrap()
            .push_str("\n[channels.pin_by_target.\"x86_64-apple-darwin\"]\ntrust = 6808\n");
        assert_eq!(levels_of(&files).unwrap(), [Coverage::Full, Coverage::Full]);
    }

    #[test]
    fn no_pinned_program_for_an_architecture_is_none() {
        let mut files = read_dir(&fixture());
        let index = files["index.toml"]
            .lines()
            .map(|l| {
                if l.starts_with("pin = ") {
                    "pin = { trust = 9192 }"
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        files.insert("index.toml".into(), index);
        assert_eq!(
            levels_of(&files).unwrap(),
            [Coverage::Absent, Coverage::Full]
        );
    }

    #[test]
    fn a_missing_pinned_manifest_is_a_refusal_never_a_guess() {
        let mut files = read_dir(&fixture());
        files.remove("pkg-claude-2026092601.toml");
        assert_eq!(
            levels_of(&files).unwrap_err(),
            Unknown::Missing {
                manifest: "pkg-claude-2026092601.toml".into(),
                triple: "x86_64-apple-darwin".into()
            }
        );
    }

    #[test]
    fn a_second_fetch_reads_no_exact_body_after_the_first_verified_them() {
        let (releases, served) = signed_fixture();
        let root = aterm_tempfile::tempdir().unwrap();
        let layout = cache_layout(&root);
        let master = public_key(&MASTER);
        let reads = Cell::new(0usize);
        let mut get = |url: &str, _: u64| {
            reads.set(reads.get() + 1);
            served
                .get(url)
                .cloned()
                .ok_or_else(|| format!("not served: {url}"))
        };
        let f = fetch(
            &releases,
            "alabsystems/aterm",
            "file:///dl/",
            Some(&layout),
            &[master.as_str()],
            NOW,
            &mut get,
        )
        .unwrap();
        assert_eq!(reads.get(), 10, "one index quad and three manifest pairs");
        assert_eq!(f.tag, "atpkg-index-50");
        assert_eq!(
            f.manifests.keys().collect::<Vec<_>>(),
            [
                "pkg-ay-8256.toml",
                "pkg-claude-2026092601.toml",
                "pkg-trust-9192.toml"
            ]
        );
        assert_eq!(
            fetched_levels(&f).unwrap(),
            [Coverage::Partial, Coverage::Full]
        );
        let blocked_reads = Cell::new(0usize);
        let mut blocked = |url: &str, _: u64| {
            blocked_reads.set(blocked_reads.get() + 1);
            Err(format!("network blocked: {url}"))
        };
        let warm = fetch(
            &releases,
            "alabsystems/aterm",
            "file:///dl/",
            Some(&layout),
            &[master.as_str()],
            NOW,
            &mut blocked,
        )
        .unwrap();
        assert_eq!(blocked_reads.get(), 0);
        assert_eq!(
            fetched_levels(&warm).unwrap(),
            [Coverage::Partial, Coverage::Full]
        );
    }

    #[test]
    fn a_corrupt_cached_object_returns_to_the_network_and_refuses_a_mismatch() {
        let (releases, served) = signed_fixture();
        let root = aterm_tempfile::tempdir().unwrap();
        let layout = cache_layout(&root);
        let master = public_key(&MASTER);
        let mut get = |url: &str, _: u64| {
            served
                .get(url)
                .cloned()
                .ok_or_else(|| format!("not served: {url}"))
        };
        fetch(
            &releases,
            "alabsystems/aterm",
            "file:///dl/",
            Some(&layout),
            &[master.as_str()],
            NOW,
            &mut get,
        )
        .unwrap();
        corrupt_objects(&layout.prefix.join("signed-artifacts/v2/objects"));
        let mut mismatched = served;
        let index_url = "file:///dl/alabsystems/aterm/releases/download/atpkg-index-50/index.toml";
        mismatched.insert(
            index_url.into(),
            b"signed bytes changed in transit".to_vec(),
        );
        let reads = Cell::new(0usize);
        let mut get_mismatch = |url: &str, _: u64| {
            reads.set(reads.get() + 1);
            mismatched
                .get(url)
                .cloned()
                .ok_or_else(|| format!("not served: {url}"))
        };
        let refused = fetch(
            &releases,
            "alabsystems/aterm",
            "file:///dl/",
            Some(&layout),
            &[master.as_str()],
            NOW,
            &mut get_mismatch,
        )
        .unwrap_err();
        assert_eq!(reads.get(), 4, "the invalid index quad was reacquired");
        assert!(refused.to_string().contains("signed index chain refused"));
    }

    #[test]
    fn no_complete_signed_index_is_no_index() {
        let root = aterm_tempfile::tempdir().unwrap();
        let layout = cache_layout(&root);
        let master = public_key(&MASTER);
        let none = parse_releases(r#"[{"tag_name":"v0.63.0","assets":[]}]"#).unwrap();
        let mut blocked = |url: &str, _: u64| Err(format!("unexpected network read: {url}"));
        assert_eq!(
            fetch(
                &none,
                "alabsystems/aterm",
                "file:///dl",
                Some(&layout),
                &[master.as_str()],
                NOW,
                &mut blocked,
            )
            .err(),
            Some(Unknown::NoIndex {
                slug: "alabsystems/aterm".into()
            })
        );
    }
}
