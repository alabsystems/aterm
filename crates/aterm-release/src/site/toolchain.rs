// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How much of the pinned ALab toolset the atpkg index publishes per Mac
//! architecture — the Intel mark's second fact (docs/DESIGN-intel-just-works-2026-09-14.md
//! §2.2, §3.3). Read from the newest `atpkg-index-<n>` release and the manifests its
//! stable channel pins; these copies are read, not verified (nothing installs from
//! them), and a pinned manifest that is missing is a refusal, never a guess.

use std::collections::BTreeMap;
use std::fmt;

use aterm_toml::Value;

use super::releases::Release;

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

/// Per Mac architecture, `{program: build}` as that darwin triple sees the stable
/// channel: `pin` with the triple's `pin_by_target` overlay laid over it (atpkg
/// `Channel::for_target`), restricted to the programs the index names.
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
    let table = |v: Option<&Value>| {
        v.and_then(Value::as_table)
            .cloned()
            .unwrap_or_else(aterm_toml::Table::new)
    };
    let mut out: [BTreeMap<String, u64>; 2] = Default::default();
    for (slot, arch) in MAC_ARCHES.iter().enumerate() {
        let mut view = table(channel.get("pin"));
        let overlay = table(table(channel.get("pin_by_target")).get(&triple(arch)));
        for (k, v) in overlay {
            view.insert(k, v);
        }
        for (program, build) in view {
            if programs.and_then(|p| p.get(&program)).is_none() {
                continue;
            }
            let name_ok = !program.is_empty()
                && program
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
            match build {
                Value::Integer(n) if name_ok && n >= 0 => {
                    out[slot].insert(program, n.unsigned_abs());
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

/// The coverage of each Mac architecture over the stable pins, reading each pinned
/// manifest through `manifest` (by name; `None` when it is not there).
///
/// # Errors
/// An index [`mac_pins`] refuses, an architecture nothing is pinned for, a missing
/// pinned manifest, or one that names another program or build.
pub fn levels(index: &Value, manifest: &dyn Fn(&str) -> Option<String>) -> Result<Levels, Unknown> {
    let mut levels = [Coverage::Absent; 2];
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
            let text = manifest(&name).ok_or_else(|| Unknown::Missing {
                manifest: name.clone(),
                triple: triple.clone(),
            })?;
            let m = parse(&text, &name)?;
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

/// The newest non-draft `atpkg-index-<n>` release that carries `index.toml`.
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
        .filter(|r| !r.draft && r.named("index.toml").is_some())
        .filter_map(|r| n(r).map(|k| (k, r)))
        .max_by_key(|(k, _)| *k)
        .map(|(_, r)| r)
}

/// What [`fetch`] read: the index release's tag, its `index.toml`, and every pinned
/// manifest by name.
pub struct Fetched {
    pub tag: String,
    pub index: String,
    pub manifests: BTreeMap<String, String>,
}

/// The newest index and every manifest its stable channel pins for a Mac, from the
/// release download host `base` (`https://github.com`, or a `file://` tree), read
/// through `get`.
///
/// # Errors
/// No index release, a failed read, or an index that names no repository for a
/// pinned program.
pub fn fetch(
    releases: &[Release],
    slug: &str,
    base: &str,
    get: &mut super::Fetch<'_>,
) -> Result<Fetched, Unknown> {
    let index_release =
        index_release(releases).ok_or_else(|| Unknown::NoIndex { slug: slug.into() })?;
    let (owner, repo) = slug.split_once('/').unwrap_or((slug, ""));
    let base = base.trim_end_matches('/');
    let mut read = |repo: &str, tag: &str, name: &str| -> Result<String, Unknown> {
        let url = format!("{base}/{owner}/{repo}/releases/download/{tag}/{name}");
        let bytes =
            get(&url, 1_000_000).map_err(|e| Unknown::Other(format!("fetch {url}: {e}")))?;
        String::from_utf8(bytes).map_err(|_| Unknown::Other(format!("fetch {url}: not UTF-8")))
    };
    let index_text = read(repo, &index_release.tag, "index.toml")?;
    let index = parse(&index_text, "index.toml")?;
    // keyed by manifest, not program: each architecture's overlay may pin its own build
    let mut wanted = std::collections::BTreeSet::new();
    for pins in mac_pins(&index)? {
        wanted.extend(pins);
    }
    let mut manifests = BTreeMap::new();
    for (program, build) in wanted {
        let repo = index
            .get("programs")
            .and_then(|p| p.get(&program))
            .and_then(|p| p.get("repo"))
            .and_then(Value::as_str)
            .filter(|r| {
                !r.is_empty()
                    && r.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
            })
            .ok_or_else(|| {
                Unknown::Other(format!("index: program {program} names no repository"))
            })?;
        let name = manifest_name(&program, build);
        let text = read(repo, &format!("atpkg-{program}-{build}"), &name)?;
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
    use std::path::{Path, PathBuf};

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site/toolchain")
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

    #[test]
    fn the_fixture_index_is_full_on_arm_and_partial_on_intel() {
        // ay and claude carry both darwin triples, trust only aarch64; a pin the index
        // names no program for is not the set's
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
    fn fetch_reads_the_newest_non_draft_index_and_only_the_pinned_manifests() {
        let releases = parse_releases(
            r#"[{"tag_name":"atpkg-index-51","prerelease":true,"draft":true,"assets":[{"name":"index.toml","size":1}]},
                {"tag_name":"atpkg-index-50","prerelease":true,"draft":false,"assets":[{"name":"index.toml","size":1},{"name":"index.toml.sig","size":64}]},
                {"tag_name":"atpkg-index-49","prerelease":true,"draft":false,"assets":[{"name":"index.toml","size":1}]}]"#,
        )
        .unwrap();
        let files = read_dir(&fixture());
        let served: BTreeMap<String, String> = [
            (
                "aterm/releases/download/atpkg-index-50/index.toml",
                "index.toml",
            ),
            (
                "ay/releases/download/atpkg-ay-8256/pkg-ay-8256.toml",
                "pkg-ay-8256.toml",
            ),
            (
                "trust/releases/download/atpkg-trust-9192/pkg-trust-9192.toml",
                "pkg-trust-9192.toml",
            ),
            (
                "aterm/releases/download/atpkg-claude-2026092601/pkg-claude-2026092601.toml",
                "pkg-claude-2026092601.toml",
            ),
        ]
        .into_iter()
        .map(|(path, file)| {
            (
                format!("file:///dl/alabsystems/{path}"),
                files[file].clone(),
            )
        })
        .collect();
        let mut get = |url: &str, _: u64| {
            served
                .get(url)
                .map(|t| t.clone().into_bytes())
                .ok_or_else(|| format!("not served: {url}"))
        };
        let f = fetch(&releases, "alabsystems/aterm", "file:///dl/", &mut get).unwrap();
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
        // an overlay pinning another build for one architecture: both builds are read,
        // and each architecture is judged on its own
        let mut overlaid = served.clone();
        let index = "file:///dl/alabsystems/aterm/releases/download/atpkg-index-50/index.toml";
        overlaid.insert(
            index.into(),
            files["index.toml"].clone()
                + "\n[channels.pin_by_target.\"x86_64-apple-darwin\"]\ntrust = 6808\n",
        );
        overlaid.insert(
            "file:///dl/alabsystems/trust/releases/download/atpkg-trust-6808/pkg-trust-6808.toml"
                .into(),
            files["pkg-trust-6808.toml"].clone(),
        );
        let mut get_overlaid = |url: &str, _: u64| {
            overlaid
                .get(url)
                .map(|t| t.clone().into_bytes())
                .ok_or_else(|| format!("not served: {url}"))
        };
        let f = fetch(
            &releases,
            "alabsystems/aterm",
            "file:///dl/",
            &mut get_overlaid,
        )
        .unwrap();
        assert_eq!(
            f.manifests.keys().collect::<Vec<_>>(),
            [
                "pkg-ay-8256.toml",
                "pkg-claude-2026092601.toml",
                "pkg-trust-6808.toml",
                "pkg-trust-9192.toml"
            ]
        );
        assert_eq!(
            fetched_levels(&f).unwrap(),
            [Coverage::Full, Coverage::Full]
        );
        let none = parse_releases(r#"[{"tag_name":"v0.63.0","assets":[]}]"#).unwrap();
        assert_eq!(
            fetch(&none, "alabsystems/aterm", "file:///dl", &mut get).err(),
            Some(Unknown::NoIndex {
                slug: "alabsystems/aterm".into()
            })
        );
    }
}
