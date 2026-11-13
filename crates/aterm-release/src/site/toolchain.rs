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
#[path = "toolchain_tests.rs"]
mod tests;
