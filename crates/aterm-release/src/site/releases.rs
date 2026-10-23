// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GitHub release list parser and download asset selection for the release site.

use std::fmt;

use aterm_json::Value;

/// The Linux architectures the site offers a download for, in attribute order.
pub const LINUX_ARCHES: [&str; 2] = ["x86_64", "aarch64"];

/// One asset of a release as the API lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    /// `None` when the API gave no byte count (absent, null, not a number, negative).
    pub size: Option<u64>,
}

/// One release as the API lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub prerelease: bool,
    pub draft: bool,
    /// `YYYY-MM-DD` of its publication (or creation), empty when the API gave none.
    pub date: String,
    pub body: String,
    pub assets: Vec<Asset>,
}

impl Release {
    /// `MAJOR.MINOR.PATCH` of a canonical `vX.Y.Z` tag.
    #[must_use]
    pub fn version_key(&self) -> Option<(u64, u64, u64)> {
        version_key(&self.tag)
    }

    /// The tag without its `v`, for a canonical tag.
    #[must_use]
    pub fn version(&self) -> &str {
        if self.version_key().is_some() {
            &self.tag[1..]
        } else {
            ""
        }
    }

    /// The asset named `name`, when exactly one carries the name.
    #[must_use]
    pub fn named(&self, name: &str) -> Option<&Asset> {
        let mut found = self.assets.iter().filter(|a| a.name == name);
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }

    /// Whether any asset carries `name`.
    #[must_use]
    pub fn lists(&self, name: &str) -> bool {
        self.assets.iter().any(|a| a.name == name)
    }

    /// The versioned URL of `name` on this release.
    #[must_use]
    pub fn download_url(&self, slug: &str, name: &str) -> String {
        format!(
            "https://github.com/{slug}/releases/download/{}/{name}",
            self.tag
        )
    }
}

/// `(MAJOR, MINOR, PATCH)` of a canonical `vX.Y.Z` tag (no leading zeros).
#[must_use]
pub fn version_key(tag: &str) -> Option<(u64, u64, u64)> {
    let mut parts = tag.strip_prefix('v')?.split('.');
    let mut next = || -> Option<u64> {
        let p = parts.next()?;
        let canonical = !p.is_empty()
            && p.bytes().all(|b| b.is_ascii_digit())
            && (p == "0" || !p.starts_with('0'));
        canonical.then(|| p.parse().ok()).flatten()
    };
    let key = (next()?, next()?, next()?);
    parts.next().is_none().then_some(key)
}

/// The versioned macOS image every app release carries.
#[must_use]
pub fn dmg_name(version: &str) -> String {
    format!("aterm-{version}.dmg")
}

/// A universal cut's record of RUNNING its x86_64 slice under Rosetta.
#[must_use]
pub fn intel_probe_name(version: &str) -> String {
    format!("aterm-{version}-x86_64-probe.txt")
}

/// The legacy Linux archive of `arch`.
#[must_use]
pub fn linux_tar_name(version: &str, arch: &str) -> String {
    format!("aterm-{version}-linux-{arch}.tar.gz")
}

/// The raw signed Linux executable of `arch`.
#[must_use]
pub fn linux_raw_name(version: &str, arch: &str) -> String {
    format!("aterm-{version}-linux-{arch}")
}

/// The size as the Finder shows it: decimal units, GB from a thousand MB.
#[must_use]
pub fn size_label(size: Option<u64>) -> Option<String> {
    #[allow(clippy::cast_precision_loss)] // a byte count far below 2^52
    let mb = size? as f64 / 1e6;
    Some(if mb >= 1000.0 {
        format!("{:.2} GB", mb / 1000.0)
    } else {
        format!("{} MB", mb.round_ties_even())
    })
}

/// Whether `slug` is an `OWNER/REPO` GitHub can name.
#[must_use]
pub fn valid_slug(slug: &str) -> bool {
    let ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
    };
    matches!(slug.split_once('/'), Some((o, r)) if ok(o) && ok(r))
}

/// Every release in `text`.
///
/// # Errors
/// Text that is not JSON, or a top-level value that is not an array or an object.
pub fn parse_releases(text: &str) -> Result<Vec<Release>, String> {
    let mut values = Vec::new();
    for doc in top_level_values(text)? {
        let value: Value = aterm_json::from_str(doc).map_err(|e| e.to_string())?;
        match value {
            Value::Array(items) => values.extend(items),
            Value::Object(_) => values.push(value),
            _ => return Err("release API must return an array or object".into()),
        }
    }
    let flat = values.into_iter().flat_map(|page| match page {
        Value::Array(items) => items,
        other => vec![other],
    });
    Ok(flat.filter_map(|r| release(&r)).collect())
}

/// The top-level JSON texts in `text`, each an array or an object.
fn top_level_values(text: &str) -> Result<Vec<&str>, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i == bytes.len() {
            return Ok(out);
        }
        if !matches!(bytes[i], b'[' | b'{') {
            return Err("release API must return an array or object".into());
        }
        let start = i;
        let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
        while i < bytes.len() {
            let b = bytes[i];
            i += 1;
            if in_string {
                match b {
                    _ if escaped => escaped = false,
                    b'\\' => escaped = true,
                    b'"' => in_string = false,
                    _ => {}
                }
                continue;
            }
            match b {
                b'"' => in_string = true,
                b'[' | b'{' => depth += 1,
                b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
        if depth != 0 {
            return Err("unexpected end of JSON input".into());
        }
        out.push(&text[start..i]);
    }
}

/// JavaScript's truthiness of a JSON value, as the API's flags are read.
fn truthy(v: Option<&Value>) -> Option<bool> {
    let v = v?;
    Some(match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    })
}

/// The first of `keys` that holds a non-empty string.
fn first_str<'v>(r: &'v Value, keys: &[&str]) -> &'v str {
    keys.iter()
        .find_map(|k| r.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()))
        .unwrap_or("")
}

/// A byte count, or `None` when the JSON carries none (absent, null, a string, a bool, a negative).
fn asset_size(v: Option<&Value>) -> Option<u64> {
    match v? {
        Value::Number(n) => n.as_u64().or_else(|| {
            n.as_f64()
                .filter(|f| *f >= 0.0 && f.is_finite())
                .map(|f| f.trunc() as u64)
        }),
        _ => None,
    }
}

/// One release object (the REST shape).
fn release(r: &Value) -> Option<Release> {
    r.as_object()?;
    let assets = r
        .get("assets")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|a| a.is_object())
                .map(|a| Asset {
                    name: a.get("name").and_then(Value::as_str).unwrap_or("").into(),
                    size: asset_size(a.get("size")),
                })
                .collect()
        })
        .unwrap_or_default();
    let date: String = first_str(
        r,
        &["published_at", "publishedAt", "created_at", "createdAt"],
    )
    .chars()
    .take(10)
    .collect();
    Some(Release {
        tag: first_str(r, &["tag_name", "tagName"]).into(),
        prerelease: truthy(r.get("prerelease"))
            .or_else(|| truthy(r.get("isPrerelease")))
            .unwrap_or(false),
        draft: truthy(r.get("draft"))
            .or_else(|| truthy(r.get("isDraft")))
            .unwrap_or(false),
        date,
        body: r.get("body").and_then(Value::as_str).unwrap_or("").into(),
        assets,
    })
}

/// Published, non-prerelease `vX.Y.Z` releases, newest version first.
#[must_use]
pub fn stable(releases: &[Release]) -> Vec<&Release> {
    let mut keep: Vec<&Release> = releases
        .iter()
        .filter(|r| !r.draft && !r.prerelease && r.version_key().is_some())
        .collect();
    keep.sort_by_key(|r| std::cmp::Reverse(r.version_key()));
    keep
}

/// The macOS download the buttons name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacDownload {
    pub tag: String,
    pub version: String,
    /// The file name, as the page shows it.
    pub label: String,
    pub url: String,
    pub size_bytes: Option<u64>,
}

impl MacDownload {
    /// The size as the page shows it.
    #[must_use]
    pub fn size_label(&self) -> Option<String> {
        size_label(self.size_bytes)
    }
}

/// How a Linux download is authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinuxKind {
    /// A raw executable whose name, digest and size a signed appcast binds.
    SignedRaw { sha256: String, size: u64 },
    /// A tarball with a `.sha256` sidecar.
    LegacyTar,
}

/// One Linux download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxDownload {
    pub tag: String,
    pub version: String,
    pub label: String,
    pub url: String,
    pub kind: LinuxKind,
}

/// Everything the pages say about the downloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub slug: String,
    pub mac: MacDownload,
    /// The tag of the newest stable release with a current appcast.
    pub head: String,
    /// The chosen release's x86_64 run record, when it carries one.
    pub intel_proof: Option<String>,
    /// Per [`LINUX_ARCHES`] entry, its download (or none).
    pub linux: [Option<LinuxDownload>; 2],
}

impl Facts {
    /// The download of `arch` (`x86_64` | `aarch64`).
    #[must_use]
    pub fn linux(&self, arch: &str) -> Option<&LinuxDownload> {
        let i = LINUX_ARCHES.iter().position(|a| *a == arch)?;
        self.linux[i].as_ref()
    }
}

/// What the election noticed on the way, said in the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// A release whose versioned DMG is there but whose app inventory is not complete.
    IncompleteApp { tag: String },
    /// A release with no versioned DMG.
    NoDmg { tag: String, name: String },
    /// The chosen release carries no x86_64 run record.
    NoIntelProof { tag: String, name: String },
    /// No stable release carries this architecture's download.
    NoLinux { arch: &'static str },
    /// The newest release with this architecture's download is older than the DMG's.
    LinuxLags {
        tag: String,
        name: String,
        arch: &'static str,
        newest: String,
    },
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncompleteApp { tag } => {
                write!(f, "{tag} has an incomplete app asset inventory — skipping")
            }
            Self::NoDmg { tag, name } => {
                write!(f, "{tag} carries no {name} (source release) — skipping")
            }
            Self::NoIntelProof { tag, name } => {
                write!(f, "{tag} carries no {name} — the Intel mark shows no proof")
            }
            Self::NoLinux { arch } => write!(
                f,
                "no stable release carries its aterm-<v>-linux-{arch}.tar.gz and checksum — no \
                 download offered"
            ),
            Self::LinuxLags {
                tag,
                name,
                arch,
                newest,
            } => write!(
                f,
                "{tag} carries no {name} yet — newest published {arch} build is {newest}"
            ),
        }
    }
}

/// Why no download could be elected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    BadSlug,
    /// No published, non-prerelease `vX.Y.0` release at all.
    NoStable {
        slug: String,
    },
    /// No stable release carries its versioned DMG with a complete app inventory.
    NothingInstallable,
    /// A raw Linux executable's signature or inventory did not verify.
    Linux {
        tag: String,
        arch: &'static str,
        why: String,
    },
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadSlug => f.write_str("invalid repository slug"),
            Self::NoStable { slug } => write!(
                f,
                "no published, non-prerelease vX.Y.Z release found for {slug}"
            ),
            Self::NothingInstallable => f.write_str(
                "no stable release carries its versioned DMG with a complete app inventory — \
                 nothing installable to link",
            ),
            Self::Linux { tag, arch, why } => write!(
                f,
                "{tag} Linux {arch} signature/asset verification refused: {why}"
            ),
        }
    }
}

/// Whether `r` is a complete app release.
fn complete_mac_app(r: &Release) -> bool {
    let v = r.version();
    [
        dmg_name(v),
        format!("aterm-{v}-mac.zip"),
        "aterm-appcast.toml".into(),
        "aterm-appcast.toml.sig".into(),
    ]
    .iter()
    .all(|n| r.named(n).is_some())
}

/// The macOS download.
///
/// # Errors
/// An invalid slug, no stable release, or none that carries a complete app.
pub fn elect_mac<'r>(
    releases: &'r [Release],
    slug: &str,
    notes: &mut Vec<Note>,
) -> Result<(MacDownload, &'r Release), Refused> {
    if !valid_slug(slug) {
        return Err(Refused::BadSlug);
    }
    let stable: Vec<&Release> = stable(releases)
        .into_iter()
        .filter(|r| r.version_key().is_some_and(|k| k.2 == 0))
        .collect();
    if stable.is_empty() {
        return Err(Refused::NoStable { slug: slug.into() });
    }
    for candidate in stable {
        let name = dmg_name(candidate.version());
        match candidate.named(&name) {
            Some(dmg) if complete_mac_app(candidate) => {
                let mac = MacDownload {
                    tag: candidate.tag.clone(),
                    version: candidate.version().into(),
                    url: candidate.download_url(slug, &name),
                    label: name,
                    size_bytes: dmg.size.filter(|s| *s > 0),
                };
                return Ok((mac, candidate));
            }
            Some(_) => notes.push(Note::IncompleteApp {
                tag: candidate.tag.clone(),
            }),
            None => notes.push(Note::NoDmg {
                tag: candidate.tag.clone(),
                name,
            }),
        }
    }
    Err(Refused::NothingInstallable)
}

/// The appcast a release's Linux executable is proved by: its current one, or a unique
/// tag-bound archived pair.
#[must_use]
pub fn linux_appcast_name(r: &Release) -> Option<String> {
    if r.lists("aterm-appcast.toml") {
        return Some("aterm-appcast.toml".into());
    }
    let archive = format!("aterm-appcast-{}.toml", r.tag);
    (r.named(&archive).is_some() && r.named(&format!("{archive}.sig")).is_some()).then_some(archive)
}

/// Proves a raw Linux executable through the signed chain (see [`super::linux`]).
pub trait LinuxProver {
    /// The signed download `release` carries for `arch`, under `authority`'s current policy.
    ///
    /// # Errors
    /// Whatever the chain refuses.
    fn prove(
        &mut self,
        release: &Release,
        arch: &'static str,
        authority: &Release,
    ) -> Result<LinuxDownload, String>;
}

/// Every download the pages name: the macOS DMG, the Intel proof, and each Linux architecture's.
///
/// # Errors
/// As [`elect_mac`], and a raw Linux executable whose proof the chain refuses.
pub fn elect(
    releases: &[Release],
    slug: &str,
    prover: &mut dyn LinuxProver,
    notes: &mut Vec<Note>,
) -> Result<Facts, Refused> {
    let (mac, chosen) = elect_mac(releases, slug, notes)?;
    let probe = intel_probe_name(chosen.version());
    let intel_proof = chosen
        .named(&probe)
        .map(|_| chosen.download_url(slug, &probe));
    if intel_proof.is_none() {
        notes.push(Note::NoIntelProof {
            tag: chosen.tag.clone(),
            name: probe,
        });
    }
    let stable: Vec<&Release> = stable(releases)
        .into_iter()
        .filter(|r| r.version_key().is_some_and(|k| k.2 == 0))
        .collect();
    let authority = stable
        .iter()
        .copied()
        .find(|r| r.lists("aterm-appcast.toml"))
        .unwrap_or(chosen);
    let mut linux: [Option<LinuxDownload>; 2] = [None, None];
    for (slot, arch) in LINUX_ARCHES.iter().enumerate() {
        let mut found = None;
        for candidate in &stable {
            let v = candidate.version();
            if candidate.lists(&linux_raw_name(v, arch))
                && linux_appcast_name(candidate).is_some()
                && candidate.version_key() <= authority.version_key()
            {
                let proved =
                    prover
                        .prove(candidate, arch, authority)
                        .map_err(|why| Refused::Linux {
                            tag: candidate.tag.clone(),
                            arch,
                            why,
                        })?;
                found = Some(proved);
                break;
            }
            let tar = linux_tar_name(v, arch);
            if candidate.named(&tar).is_some()
                && candidate.named(&format!("{tar}.sha256")).is_some()
            {
                found = Some(LinuxDownload {
                    tag: candidate.tag.clone(),
                    version: v.into(),
                    url: candidate.download_url(slug, &tar),
                    label: tar,
                    kind: LinuxKind::LegacyTar,
                });
                break;
            }
        }
        match &found {
            None => notes.push(Note::NoLinux { arch }),
            Some(d) if d.kind == LinuxKind::LegacyTar && d.version != mac.version => {
                notes.push(Note::LinuxLags {
                    tag: mac.tag.clone(),
                    name: linux_tar_name(&mac.version, arch),
                    arch,
                    newest: d.tag.clone(),
                });
            }
            Some(_) => {}
        }
        linux[slot] = found;
    }
    Ok(Facts {
        slug: slug.into(),
        mac,
        head: authority.tag.clone(),
        intel_proof,
        linux,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str, size: Option<u64>) -> Asset {
        Asset {
            name: name.into(),
            size,
        }
    }

    fn rel(tag: &str, assets: &[(&str, Option<u64>)]) -> Release {
        Release {
            tag: tag.into(),
            prerelease: false,
            draft: false,
            date: String::new(),
            body: String::new(),
            assets: assets.iter().map(|(n, s)| asset(n, *s)).collect(),
        }
    }

    /// A complete app release of `v` whose DMG weighs `size`.
    fn app(v: &str, size: Option<u64>) -> Release {
        let mut r = rel(&format!("v{v}"), &[]);
        r.assets = vec![
            asset(&dmg_name(v), size),
            asset(&format!("aterm-{v}-mac.zip"), Some(1)),
            asset("aterm-appcast.toml", Some(1)),
            asset("aterm-appcast.toml.sig", Some(64)),
        ];
        r
    }

    struct NoProver;
    impl LinuxProver for NoProver {
        fn prove(
            &mut self,
            r: &Release,
            _: &'static str,
            _: &Release,
        ) -> Result<LinuxDownload, String> {
            Err(format!("no prover for {}", r.tag))
        }
    }

    const SLUG: &str = "alabsystems/aterm";

    #[test]
    fn the_size_is_the_finders_decimal_units() {
        let label = |n| size_label(Some(n)).unwrap();
        assert_eq!(label(1_067_044_990), "1.07 GB");
        assert_eq!(label(28_219_832), "28 MB");
        assert_eq!(label(999_400_000), "999 MB");
        assert_eq!(label(1_000_000_000), "1.00 GB");
        assert_eq!(label(12_345_678_901), "12.35 GB");
        assert_eq!(size_label(None), None);
    }

    #[test]
    fn the_newest_version_wins_not_githubs_latest_and_the_url_is_versioned() {
        let releases = vec![
            app("0.63.0", Some(1_067_044_990)),
            app("0.64.0", Some(31_457_280)),
        ];
        let (mac, _) = elect_mac(&releases, SLUG, &mut Vec::new()).unwrap();
        assert_eq!(mac.label, "aterm-0.64.0.dmg");
        assert_eq!(
            mac.url,
            "https://github.com/alabsystems/aterm/releases/download/v0.64.0/aterm-0.64.0.dmg"
        );
        assert_eq!(mac.size_label().as_deref(), Some("31 MB"));
    }

    #[test]
    fn a_source_release_atop_an_app_cut_is_skipped_and_named() {
        let releases = vec![
            rel("v0.64.0", &[("SHA256SUMS", Some(273_281))]),
            app("0.63.0", Some(1_067_044_990)),
        ];
        let mut notes = Vec::new();
        let (mac, _) = elect_mac(&releases, SLUG, &mut notes).unwrap();
        assert_eq!(mac.tag, "v0.63.0");
        assert_eq!(
            notes,
            vec![Note::NoDmg {
                tag: "v0.64.0".into(),
                name: "aterm-0.64.0.dmg".into()
            }]
        );
    }

    #[test]
    fn an_incomplete_app_upload_never_wins() {
        let mut half = app("0.64.0", Some(1));
        half.assets.retain(|a| a.name != "aterm-appcast.toml.sig");
        let releases = vec![half, app("0.63.0", Some(2))];
        let mut notes = Vec::new();
        let (mac, _) = elect_mac(&releases, SLUG, &mut notes).unwrap();
        assert_eq!(mac.tag, "v0.63.0");
        assert_eq!(
            notes,
            vec![Note::IncompleteApp {
                tag: "v0.64.0".into()
            }]
        );
    }

    #[test]
    fn a_stream_with_no_versioned_dmg_anywhere_is_refused() {
        let releases = vec![rel(
            "v0.65.0",
            &[
                ("aterm.dmg", Some(31_457_280)),
                ("aterm-0.65.0-mac.zip", Some(1)),
            ],
        )];
        let mut notes = Vec::new();
        assert_eq!(
            elect_mac(&releases, SLUG, &mut notes).unwrap_err(),
            Refused::NothingInstallable
        );
        assert_eq!(
            notes,
            vec![Note::NoDmg {
                tag: "v0.65.0".into(),
                name: "aterm-0.65.0.dmg".into()
            }]
        );
        assert_eq!(
            elect_mac(&[], SLUG, &mut notes).unwrap_err(),
            Refused::NoStable { slug: SLUG.into() }
        );
        assert_eq!(
            elect_mac(&releases, "no slug", &mut notes).unwrap_err(),
            Refused::BadSlug
        );
    }

    #[test]
    fn an_unknown_size_decides_nothing() {
        for json in [
            r#"[{"tag_name":"v0.63.0","assets":[{"name":"aterm-0.63.0.dmg"}]}]"#,
            r#"[{"tag_name":"v0.63.0","assets":[{"name":"aterm-0.63.0.dmg","size":"big"}]}]"#,
            r#"[{"tag_name":"v0.63.0","assets":[{"name":"aterm-0.63.0.dmg","size":null}]}]"#,
        ] {
            let mut r = parse_releases(json).unwrap();
            r[0].assets
                .extend(app("0.63.0", None).assets.into_iter().skip(1));
            let (mac, _) = elect_mac(&r, SLUG, &mut Vec::new()).unwrap();
            assert_eq!(
                (mac.label.as_str(), mac.size_bytes),
                ("aterm-0.63.0.dmg", None),
                "{json}"
            );
        }
    }

    #[test]
    fn every_json_shape_the_api_and_gh_print_is_read() {
        let one = r#"{"tagName":"v0.64.0","isPrerelease":false,"assets":[{"name":"x","size":3,"url":"u"}]}"#;
        let pages = r#"[{"tag_name":"v0.2.0"}]
            [{"tag_name":"v0.1.0","draft":true}]"#;
        let slurped = r#"[[{"tag_name":"v0.2.0"}],[{"tag_name":"v0.1.0"}]]"#;
        assert_eq!(
            parse_releases(one).unwrap()[0].assets,
            vec![asset("x", Some(3))]
        );
        let paged = parse_releases(pages).unwrap();
        assert_eq!(
            paged
                .iter()
                .map(|r| (r.tag.as_str(), r.draft))
                .collect::<Vec<_>>(),
            [("v0.2.0", false), ("v0.1.0", true)]
        );
        assert_eq!(parse_releases(slurped).unwrap().len(), 2);
        assert!(parse_releases("3").is_err());
        assert!(parse_releases("[{").is_err());
    }

    #[test]
    fn stable_is_published_canonical_tags_newest_first() {
        let mut rc = rel("v0.64.0-rc.1", &[]);
        rc.prerelease = true;
        let releases = vec![
            rel("v0.61.0", &[]),
            rc,
            rel("atpkg-index-7", &[]),
            rel("v0.63.0", &[]),
            rel("v0.010.0", &[]),
        ];
        let tags: Vec<&str> = stable(&releases).iter().map(|r| r.tag.as_str()).collect();
        assert_eq!(tags, ["v0.63.0", "v0.61.0"]);
    }

    #[test]
    fn linux_follows_the_newest_release_that_carries_its_tarball() {
        let mut old = app("0.63.0", Some(1));
        old.assets
            .push(asset("aterm-0.63.0-linux-x86_64.tar.gz", Some(15_887_638)));
        old.assets
            .push(asset("aterm-0.63.0-linux-x86_64.tar.gz.sha256", Some(99)));
        let releases = vec![app("0.64.0", Some(1)), old];
        let mut notes = Vec::new();
        let facts = elect(&releases, SLUG, &mut NoProver, &mut notes).unwrap();
        assert_eq!(facts.head, "v0.64.0");
        let x86 = facts.linux("x86_64").unwrap();
        assert_eq!(
            (x86.tag.as_str(), x86.label.as_str(), &x86.kind),
            (
                "v0.63.0",
                "aterm-0.63.0-linux-x86_64.tar.gz",
                &LinuxKind::LegacyTar
            )
        );
        assert_eq!(facts.linux("aarch64"), None);
        assert!(notes.contains(&Note::LinuxLags {
            tag: "v0.64.0".into(),
            name: "aterm-0.64.0-linux-x86_64.tar.gz".into(),
            arch: "x86_64",
            newest: "v0.63.0".into()
        }));
        assert!(notes.contains(&Note::NoLinux { arch: "aarch64" }));
        let mut bare = app("0.64.0", Some(1));
        bare.assets
            .push(asset("aterm-0.64.0-linux-x86_64.tar.gz", Some(1)));
        let facts = elect(&[bare], SLUG, &mut NoProver, &mut Vec::new()).unwrap();
        assert_eq!(facts.linux("x86_64"), None);
    }

    #[test]
    fn a_raw_linux_executable_is_proved_or_the_election_refuses() {
        let mut r = app("0.64.0", Some(1));
        r.assets
            .push(asset("aterm-0.64.0-linux-aarch64", Some(9000)));
        let err = elect(&[r], SLUG, &mut NoProver, &mut Vec::new()).unwrap_err();
        assert_eq!(
            err,
            Refused::Linux {
                tag: "v0.64.0".into(),
                arch: "aarch64",
                why: "no prover for v0.64.0".into()
            }
        );
    }

    #[test]
    fn the_intel_proof_is_the_chosen_releases_own_record() {
        let mut r = app("0.64.0", Some(1));
        r.assets
            .push(asset("aterm-0.64.0-x86_64-probe.txt", Some(900)));
        let facts = elect(&[r], SLUG, &mut NoProver, &mut Vec::new()).unwrap();
        assert_eq!(
            facts.intel_proof.as_deref(),
            Some(
                "https://github.com/alabsystems/aterm/releases/download/v0.64.0/\
                 aterm-0.64.0-x86_64-probe.txt"
            )
        );
        let mut r = app("0.64.0", Some(1));
        r.assets
            .push(asset("aterm-0.63.0-x86_64-probe.txt", Some(900)));
        let mut notes = Vec::new();
        let facts = elect(&[r], SLUG, &mut NoProver, &mut notes).unwrap();
        assert_eq!(facts.intel_proof, None);
        assert!(notes.contains(&Note::NoIntelProof {
            tag: "v0.64.0".into(),
            name: "aterm-0.64.0-x86_64-probe.txt".into()
        }));
    }
}
