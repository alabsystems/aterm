// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The vendor-direct resolver (design §1.1–§1.2): the vendor's head, then its digest
//! documents — each fetched under the program's pins and caps, each landing where the
//! table allows — authenticated by the program's anchor into ONE candidate build.
//!
//! The head is a hint: it names a version and nothing else is taken from it until the
//! anchor has vouched for the document that carries the digest. The platform and the
//! asset are chosen from the client's own triple, never from a document.

use std::collections::{BTreeMap, BTreeSet};

use super::digests::AuthenticatedDigest;
use super::table::{Anchor, VendorSpec};
use super::version::{BuildDate, Version};
use crate::flow::{Fetcher, FlowError, VendorFetchError, VendorGet};
use crate::manifest::{Artifact, Cost};
use crate::openpgp::PinnedRsaKey;

/// The claude head: `x.y.z` and an optional newline.
const CLAUDE_HEAD_CAP: u64 = 64;
/// The codex head IS the release JSON.
const CODEX_HEAD_CAP: u64 = 1024 * 1024;
/// `<v>/manifest.json`.
const MANIFEST_CAP: u64 = 64 * 1024;
/// `codex-package_SHA256SUMS`.
const SUMS_CAP: u64 = 64 * 1024;
/// A gzip'd codex package unpacks to at most this many times its size (0.156.0 on
/// darwin: 127 MB → 318 MB); the extraction cap is twice what it yields.
const ARCHIVE_UNPACK_RATIO: u64 = 4;
/// Where every codex payload and `SHA256SUMS` is fetched.
const CODEX_DOWNLOADS: &str = "https://github.com/openai/codex/releases/download/";

/// What the head said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Head {
    /// 304 to the conditional GET: the head is the one last seen.
    Unchanged {
        /// The validator to keep.
        etag: String,
    },
    /// The host was not reached, or is not serving now.
    Unreachable(String),
    /// The host answered, but not with the head: a status, a TLS refusal, a body over its
    /// cap. It says nothing about any build.
    Unserved(String),
    /// The head could not be read as a canonical version, or landed off its pin.
    Refused(String),
    /// The head names `version`; `body` is the whole document (codex's is its release JSON).
    Named {
        /// The named version.
        version: Version,
        /// The response's validator.
        etag: Option<String>,
        /// The document.
        body: Vec<u8>,
    },
}

/// A payload row the vendor lane built from authenticated documents. It has no public
/// constructor, so no index row can become one; [`admit`] checks it before any byte moves.
#[derive(Debug, Clone)]
pub struct SyntheticArtifact {
    program: &'static str,
    version: Version,
    artifact: Artifact,
}

impl SyntheticArtifact {
    /// The row the stage lanes read.
    #[must_use]
    pub(crate) const fn artifact(&self) -> &Artifact {
        &self.artifact
    }
}

/// One authenticated candidate build.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    /// Its version (the head's).
    pub version: Version,
    /// The payload row.
    pub artifact: SyntheticArtifact,
    /// The signed `buildDate` (claude only).
    pub build_date: Option<BuildDate>,
    /// What authenticated the digest.
    pub anchor: Anchor,
    /// The payload's authenticated sha256.
    pub digest: AuthenticatedDigest,
}

/// What verifying a head's documents came to.
#[derive(Debug, Clone)]
pub(crate) enum Resolution {
    /// A vendor host was not reached; keep what is installed, quietly.
    Unreachable,
    /// A vendor host answered, but not with the document (a 404 while the vendor is still
    /// publishing, a TLS refusal): nothing about the bytes, so kept at like `Unreachable`.
    Unserved(String),
    /// A document was refused (never a payload: nothing was downloaded).
    Refused {
        /// The version the head named, when it named one.
        version: Option<Version>,
        /// Why, in one line.
        reason: String,
    },
    /// A verified candidate.
    Candidate(Box<Candidate>),
}

/// A vendor document GET in [`Fetcher::vendor_get`]'s shape: `(program, url, cap,
/// if_none_match)`. The window's head watch reads heads through it, under the lane's pins
/// and caps.
pub type VendorGetFn<'a> =
    dyn Fn(&str, &str, u64, Option<&str>) -> Result<VendorGet, VendorFetchError> + 'a;

/// GET `spec`'s head, conditionally on `if_none_match`, in the short single attempt of
/// [`Fetcher::vendor_head`]: an unreached head keeps the installed build, and the next
/// pass or check asks again.
pub(crate) fn read_head(
    spec: &VendorSpec,
    fetcher: &dyn Fetcher,
    if_none_match: Option<&str>,
) -> Head {
    read_head_via(
        spec,
        &|program, url, cap, etag| fetcher.vendor_head(program, url, cap, etag),
        if_none_match,
    )
}

/// The head's document asked again, unconditionally and patiently
/// ([`Fetcher::vendor_get`]): after a 304 the head was read, and the lane needs the
/// document it names to verify against.
pub(crate) fn reread_head(spec: &VendorSpec, fetcher: &dyn Fetcher) -> Head {
    read_head_via(
        spec,
        &|program, url, cap, etag| fetcher.vendor_get(program, url, cap, etag),
        None,
    )
}

/// [`read_head`] over a bare GET: the head's cap, its pin and its parse are the same.
pub(crate) fn read_head_via(
    spec: &VendorSpec,
    get: &VendorGetFn<'_>,
    if_none_match: Option<&str>,
) -> Head {
    let cap = match spec.anchor {
        Anchor::AnthropicOpenPgp => CLAUDE_HEAD_CAP,
        Anchor::OpenAiTwoHost => CODEX_HEAD_CAP,
    };
    let (body, etag) = match get(spec.program, spec.head_url, cap, if_none_match) {
        Ok(VendorGet::NotModified { etag }) => return Head::Unchanged { etag },
        Ok(VendorGet::Body {
            bytes,
            etag,
            effective_url,
        }) => {
            if !spec.digest_doc_admitted(spec.head_url, &effective_url) {
                return Head::Refused(off_pin(spec.head_url, &effective_url));
            }
            (bytes, etag)
        }
        Err(VendorFetchError::Unreachable(why)) => return Head::Unreachable(why),
        Err(VendorFetchError::Refused(why)) => return Head::Unserved(why),
    };
    let named = match spec.anchor {
        Anchor::AnthropicOpenPgp => claude_head_version(&body),
        Anchor::OpenAiTwoHost => strict_json(&body)
            .ok()
            .and_then(|doc| codex_tag_version(&doc).ok()),
    };
    match named {
        Some(version) => Head::Named {
            version,
            etag,
            body,
        },
        None => Head::Refused(String::from(
            "the release channel's head does not name a canonical x.y.z version",
        )),
    }
}

/// Verify the documents behind `version` (the head just named it; `head_body` is the
/// head's document) into a candidate for `triple`, under `key` (claude's anchor) and the
/// clock `now`.
pub(crate) fn verify(
    spec: &'static VendorSpec,
    triple: &str,
    fetcher: &dyn Fetcher,
    version: Version,
    head_body: &[u8],
    key: &PinnedRsaKey,
    now: i64,
) -> Resolution {
    let verified = match spec.anchor {
        Anchor::AnthropicOpenPgp => verify_claude(spec, triple, fetcher, version, key, now),
        Anchor::OpenAiTwoHost => verify_codex(spec, triple, fetcher, version, head_body),
    };
    match verified {
        Ok(candidate) => Resolution::Candidate(Box::new(candidate)),
        Err(Fault::Unreachable) => Resolution::Unreachable,
        Err(Fault::Unserved(why)) => Resolution::Unserved(why),
        Err(Fault::Refused(reason)) => Resolution::Refused {
            version: Some(version),
            reason,
        },
    }
}

/// Admit a synthetic row before any byte moves: its shape is exactly what [`verify`]
/// builds for its program — the https payload lane its program uses, a URL under that
/// program's pins, a sha256 and an exact size, the per-version local name, and nothing
/// the table does not supply (no links, no strip, no signed root to compare).
///
/// # Errors
/// [`FlowError::VendorRefused`] naming the first field that does not fit.
pub(crate) fn admit(row: &SyntheticArtifact) -> Result<(), FlowError> {
    let refuse = |why: &str| Err(FlowError::VendorRefused(why.to_string()));
    let Some(spec) = super::spec(row.program) else {
        return refuse("not a vendor-direct program");
    };
    let a = &row.artifact;
    if a.kind != "binary" || a.protocol != "https" {
        return refuse("a vendor-direct row is an https binary");
    }
    if !crate::vendor::vendor_direct_url_allowed(row.program, &a.url) {
        return refuse("url is outside the program's pinned vendor-direct prefixes");
    }
    let lane_ok = match spec.anchor {
        Anchor::AnthropicOpenPgp => {
            a.payload == "raw-binary" && spec.exposes.contains(&a.entry.as_str())
        }
        Anchor::OpenAiTwoHost => {
            matches!(a.payload.as_str(), "tar-gz" | "zip") && a.entry.is_empty()
        }
    };
    if !lane_ok {
        return refuse("payload lane or entry is not the program's");
    }
    if a.strip_components != 0 || !a.links.is_empty() || !a.tree_root.is_empty() {
        return refuse("a vendor-direct row carries no strip, links or signed root");
    }
    if !super::is_hex64(&a.sha256) || a.size == 0 {
        return refuse("a vendor-direct row needs a sha256 and an exact size");
    }
    if a.asset != asset_name(row.program, row.version, &a.target, &a.payload) {
        return refuse("asset is not the per-version local name");
    }
    Ok(())
}

/// `<program>-<version>-<triple>[.ext]`: the staging name, per version, so a `.part` left
/// by one version is never resumed as another.
fn asset_name(program: &str, version: Version, triple: &str, payload: &str) -> String {
    let ext = match payload {
        "tar-gz" => ".tar.gz",
        "zip" => ".zip",
        _ if triple.contains("-windows-") => ".exe",
        _ => "",
    };
    format!("{program}-{version}-{triple}{ext}")
}

/// Why a verification stopped.
enum Fault {
    Unreachable,
    Unserved(String),
    Refused(String),
}

/// A fetch the transport refused is the host not serving the document, never a verdict
/// on it: only what the documents themselves say is a refusal.
impl From<VendorFetchError> for Fault {
    fn from(e: VendorFetchError) -> Self {
        match e {
            VendorFetchError::Unreachable(_) => Self::Unreachable,
            VendorFetchError::Refused(why) => Self::Unserved(why),
        }
    }
}

fn refused<T>(why: impl Into<String>) -> Result<T, Fault> {
    Err(Fault::Refused(why.into()))
}

/// GET one digest document: whole, under `cap`, unconditionally, and answered where the
/// table allows it to land.
fn digest_doc(
    spec: &VendorSpec,
    fetcher: &dyn Fetcher,
    url: &str,
    cap: u64,
) -> Result<Vec<u8>, Fault> {
    match fetcher.vendor_get(spec.program, url, cap, None)? {
        VendorGet::Body {
            bytes,
            effective_url,
            ..
        } => {
            if crate::vendor::digest_effective_url_ok(spec.program, url, &effective_url) {
                Ok(bytes)
            } else {
                refused(off_pin(url, &effective_url))
            }
        }
        VendorGet::NotModified { .. } => refused("an unconditional GET answered 304"),
    }
}

fn off_pin(requested: &str, effective: &str) -> String {
    format!("{requested} was answered from {effective}, off its pinned host")
}

/// `x.y.z` and at most one trailing newline, canonical.
fn claude_head_version(body: &[u8]) -> Option<Version> {
    let text = std::str::from_utf8(body).ok()?;
    Version::parse(text.strip_suffix('\n').unwrap_or(text))
}

/// Anthropic's `manifest.json` platform key for the client's own triple: glibc names on
/// linux, the musl ones only for a musl client.
fn claude_platform(triple: &str) -> Option<&'static str> {
    Some(match triple {
        "aarch64-apple-darwin" => "darwin-arm64",
        "x86_64-apple-darwin" => "darwin-x64",
        "x86_64-unknown-linux-gnu" => "linux-x64",
        "aarch64-unknown-linux-gnu" => "linux-arm64",
        "x86_64-unknown-linux-musl" => "linux-x64-musl",
        "aarch64-unknown-linux-musl" => "linux-arm64-musl",
        "x86_64-pc-windows-msvc" => "win32-x64",
        "aarch64-pc-windows-msvc" => "win32-arm64",
        _ => return None,
    })
}

/// The base every claude document hangs under: the head URL without its `latest`.
fn claude_base(spec: &VendorSpec) -> &'static str {
    let head = spec.head_url;
    head.rfind('/').map_or(head, |i| &head[..=i])
}

fn verify_claude(
    spec: &'static VendorSpec,
    triple: &str,
    fetcher: &dyn Fetcher,
    version: Version,
    key: &PinnedRsaKey,
    now: i64,
) -> Result<Candidate, Fault> {
    let Some(platform) = claude_platform(triple) else {
        return refused(format!("Anthropic publishes no claude build for {triple}"));
    };
    let base = claude_base(spec);
    let manifest_url = format!("{base}{version}/manifest.json");
    let manifest = digest_doc(spec, fetcher, &manifest_url, MANIFEST_CAP)?;
    let sig_url = format!("{manifest_url}.sig");
    let sig = digest_doc(
        spec,
        fetcher,
        &sig_url,
        crate::openpgp::MAX_SIGNATURE_LEN as u64,
    )?;
    let clock = u64::try_from(now).unwrap_or(0);
    if let Err(e) = crate::openpgp::verify_detached(&manifest, &sig, key, clock) {
        return refused(format!(
            "manifest.json is not signed by Anthropic's key: {e}"
        ));
    }
    // Parsed from the verified bytes only.
    let Ok(doc) = strict_json(&manifest) else {
        return refused(
            "the signed manifest.json is not strict JSON (a key repeats or it does not parse)",
        );
    };
    if doc.get("version").and_then(aterm_json::Value::as_str) != Some(version.to_string().as_str())
    {
        return refused(format!("the signed manifest.json is not for {version}"));
    }
    let build_date = match doc.get("buildDate") {
        None => None,
        Some(v) => match v.as_str().and_then(BuildDate::parse) {
            Some(d) => Some(d),
            None => return refused("the signed manifest.json's buildDate is not a UTC instant"),
        },
    };
    let Some(row) = doc.pointer(&format!("/platforms/{platform}")) else {
        return refused(format!(
            "the signed manifest.json names no {platform} build"
        ));
    };
    let windows = triple.contains("-windows-");
    let binary = row.get("binary").and_then(aterm_json::Value::as_str);
    let binary_ok = binary == Some("claude") || (windows && binary == Some("claude.exe"));
    let Some(binary) = binary.filter(|_| binary_ok) else {
        return refused(format!(
            "the signed manifest.json names another binary for {platform}"
        ));
    };
    let checksum = row.get("checksum").and_then(aterm_json::Value::as_str);
    let Some(digest) = checksum
        .filter(|c| c.len() == 64)
        .and_then(AuthenticatedDigest::authenticated)
    else {
        return refused(format!(
            "the signed manifest.json carries no sha256 for {platform}"
        ));
    };
    let Some(size) = row
        .get("size")
        .and_then(aterm_json::Value::as_u64)
        .filter(|&s| s > 0)
    else {
        return refused(format!(
            "the signed manifest.json carries no size for {platform}"
        ));
    };
    let url = format!("{base}{version}/{platform}/{binary}");
    let entry = spec.exposes.first().copied().unwrap_or(spec.program);
    let artifact = synthetic(
        spec,
        version,
        triple,
        &url,
        &digest,
        size,
        "raw-binary",
        entry,
        size,
    );
    Ok(Candidate {
        version,
        artifact,
        build_date,
        anchor: spec.anchor,
        digest,
    })
}

fn verify_codex(
    spec: &'static VendorSpec,
    triple: &str,
    fetcher: &dyn Fetcher,
    version: Version,
    release_json: &[u8],
) -> Result<Candidate, Fault> {
    let Ok(release) = strict_json(release_json) else {
        return refused("the release JSON is not strict JSON (a key repeats or it does not parse)");
    };
    match codex_tag_version(&release) {
        Ok(tagged) if tagged == version => {}
        _ => {
            return refused(format!(
                "the release JSON's tag_name is not rust-v{version}"
            ));
        }
    }
    let tag = format!("rust-v{version}");
    let sums_url = format!("{CODEX_DOWNLOADS}{tag}/codex-package_SHA256SUMS");
    let sums_text = digest_doc(spec, fetcher, &sums_url, SUMS_CAP)?;
    let Some(sums) = parse_sums(&sums_text) else {
        return refused(
            "codex-package_SHA256SUMS is malformed (a line is not `<sha256>  <name>`, or a name repeats)",
        );
    };
    let Some((asset, from_sums)) = codex_candidates(triple)
        .into_iter()
        .find_map(|name| sums.get(&name).map(|d| (name, d.clone())))
    else {
        return refused(format!(
            "OpenAI's SHA256SUMS for {tag} lists no package for {triple}"
        ));
    };
    let from_json = match json_asset_digest(&release, &asset) {
        Ok(d) => d,
        Err(why) => return refused(why),
    };
    if from_json != from_sums {
        return refused(format!(
            "the release JSON and SHA256SUMS disagree on {asset}'s sha256"
        ));
    }
    let Some(digest) = AuthenticatedDigest::authenticated(&from_sums) else {
        return refused(format!("{asset}'s sha256 is not 64 hex digits"));
    };
    let url = format!("{CODEX_DOWNLOADS}{tag}/{asset}");
    if !url.contains(&format!("/{tag}/")) {
        return refused(format!("{url} is not under the {tag} release"));
    }
    let size = fetcher.vendor_content_length(spec.program, &url)?;
    if size == 0 {
        return refused(format!("{url} answered no content length"));
    }
    let payload = if asset.ends_with(".zip") {
        "zip"
    } else {
        "tar-gz"
    };
    let artifact = synthetic(
        spec,
        version,
        triple,
        &url,
        &digest,
        size,
        payload,
        "",
        size.saturating_mul(ARCHIVE_UNPACK_RATIO),
    );
    Ok(Candidate {
        version,
        artifact,
        build_date: None,
        anchor: spec.anchor,
        digest,
    })
}

/// The row [`admit`] expects for `spec` at `version` on `triple`.
#[allow(
    clippy::too_many_arguments,
    reason = "one field per argument of the row the two anchors build"
)]
fn synthetic(
    spec: &'static VendorSpec,
    version: Version,
    triple: &str,
    url: &str,
    digest: &AuthenticatedDigest,
    size: u64,
    payload: &str,
    entry: &str,
    disk_installed: u64,
) -> SyntheticArtifact {
    SyntheticArtifact {
        program: spec.program,
        version,
        artifact: Artifact {
            target: triple.to_string(),
            kind: "binary".into(),
            protocol: "https".into(),
            asset: asset_name(spec.program, version, triple, payload),
            sha256: digest.as_str().to_string(),
            tree_root: String::new(),
            size,
            reloc: String::from("self-contained"),
            cost: Cost {
                download_bytes: size,
                disk_installed,
                build_seconds: 0,
            },
            url: url.to_string(),
            payload: payload.to_string(),
            entry: entry.to_string(),
            strip_components: 0,
            links: BTreeMap::new(),
            vendor: spec.vendor.to_string(),
        },
    }
}

/// The version a codex release JSON's `tag_name` (`rust-v<x.y.z>`) names.
fn codex_tag_version(doc: &aterm_json::Value) -> Result<Version, ()> {
    doc.get("tag_name")
        .and_then(aterm_json::Value::as_str)
        .and_then(|t| t.strip_prefix("rust-v"))
        .and_then(Version::parse)
        .ok_or(())
}

/// The publisher's asset rule for `triple` (the deleted authoring ceremony's
/// `resolve_codex`, kept here as the one statement of it), in preference order: the tarball everywhere; on windows the zip
/// first; on linux the static musl package as the glibc triple's fallback.
fn codex_candidates(triple: &str) -> Vec<String> {
    let package = |t: &str, ext: &str| format!("codex-package-{t}{ext}");
    if triple.contains("-windows-") {
        vec![package(triple, ".zip"), package(triple, ".tar.gz")]
    } else if let Some(stem) = triple
        .strip_suffix("-gnu")
        .filter(|_| triple.contains("-linux-"))
    {
        vec![
            package(triple, ".tar.gz"),
            package(&format!("{stem}-musl"), ".tar.gz"),
        ]
    } else {
        vec![package(triple, ".tar.gz")]
    }
}

/// `SHA256SUMS`, strictly: every non-empty line is 64 hex digits, one or two spaces (the
/// second may be `*`, binary mode), and a bare file name; no name twice. Digests come
/// back lowercase.
fn parse_sums(text: &[u8]) -> Option<BTreeMap<String, String>> {
    let text = std::str::from_utf8(text).ok()?;
    let mut out = BTreeMap::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let (digest, rest) = (line.get(..64)?, line.get(64..)?);
        if !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let name = rest
            .strip_prefix("  ")
            .or_else(|| rest.strip_prefix(" *"))?;
        let bare = !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if !bare
            || out
                .insert(name.to_string(), digest.to_ascii_lowercase())
                .is_some()
        {
            return None;
        }
    }
    Some(out)
}

/// The `sha256:<hex>` digest the release JSON's ONE asset named `name` carries.
fn json_asset_digest(release: &aterm_json::Value, name: &str) -> Result<String, String> {
    let assets = release
        .get("assets")
        .and_then(aterm_json::Value::as_array)
        .ok_or_else(|| String::from("the release JSON lists no assets"))?;
    let mut matching = assets
        .iter()
        .filter(|a| a.get("name").and_then(aterm_json::Value::as_str) == Some(name));
    let (Some(asset), None) = (matching.next(), matching.next()) else {
        return Err(format!(
            "the release JSON does not list {name} exactly once"
        ));
    };
    asset
        .get("digest")
        .and_then(aterm_json::Value::as_str)
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| format!("the release JSON carries no sha256 digest for {name}"))
}

/// JSON with every object's keys distinct: a document whose meaning depends on which of
/// two equal keys a parser keeps is refused, never read.
fn strict_json(bytes: &[u8]) -> Result<aterm_json::Value, ()> {
    aterm_json::from_slice::<Strict>(bytes)
        .map(|s| s.0)
        .map_err(|_| ())
}

/// A [`aterm_json::Value`] built by a visitor that refuses a repeated key.
struct Strict(aterm_json::Value);

impl<'de> serde::Deserialize<'de> for Strict {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(StrictVisitor).map(Strict)
    }
}

struct StrictVisitor;

impl<'de> serde::de::Visitor<'de> for StrictVisitor {
    type Value = aterm_json::Value;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JSON with no repeated key")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(aterm_json::Value::Null)
    }

    fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
        Ok(aterm_json::Value::Bool(v))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
        Ok(aterm_json::Value::Number(v.into()))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
        Ok(aterm_json::Value::Number(v.into()))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E> {
        Ok(aterm_json::Value::Number(v.into()))
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
        Ok(aterm_json::Value::String(v.to_string()))
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
        Ok(aterm_json::Value::String(v))
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut out = Vec::new();
        while let Some(Strict(v)) = seq.next_element()? {
            out.push(v);
        }
        Ok(aterm_json::Value::Array(out))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut seen = BTreeSet::new();
        let mut out = aterm_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(serde::de::Error::custom("a key repeats"));
            }
            let Strict(v) = map.next_value()?;
            out.insert(key, v);
        }
        Ok(aterm_json::Value::Object(out))
    }
}

#[cfg(test)]
mod tests {
    use super::super::lane::world::{CLAUDE, CODEX_DL, CODEX_HEAD, Fake};
    use super::*;

    const REAL_MANIFEST: &[u8] = include_bytes!("fixtures/claude-2.1.280-manifest.json");
    const REAL_SIG: &[u8] = include_bytes!("fixtures/claude-2.1.280-manifest.json.sig");
    const REAL_RELEASE: &[u8] = include_bytes!("fixtures/codex-0.156.0-release.json");
    const REAL_SUMS: &[u8] = include_bytes!("fixtures/codex-0.156.0-SHA256SUMS");
    /// 2026-09-22T00:00:00Z, after both real releases were signed.
    const NOW: i64 = 1_790_035_200;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn spec(program: &str) -> &'static VendorSpec {
        super::super::spec(program).unwrap()
    }

    fn candidate(r: Resolution) -> Candidate {
        match r {
            Resolution::Candidate(c) => *c,
            other => panic!("expected a candidate, got {other:?}"),
        }
    }

    /// THE REAL DOCUMENTS, UNDER THE COMPILED KEY: Anthropic's own 2.1.280 manifest and
    /// signature resolve, for every platform the client can be, to the digest, size and
    /// binary name Anthropic signed — and the URL the lane will fetch from.
    #[test]
    fn the_real_claude_release_resolves_under_the_compiled_key() {
        let f = Fake::default();
        f.doc(&format!("{CLAUDE}2.1.280/manifest.json"), REAL_MANIFEST);
        f.doc(&format!("{CLAUDE}2.1.280/manifest.json.sig"), REAL_SIG);
        let key = crate::openpgp::ANTHROPIC_CLAUDE_CODE_RELEASE_KEY;
        for (triple, platform, sha, size, suffix) in [
            (
                "aarch64-apple-darwin",
                "darwin-arm64",
                "387a5c5dcdbb815085edf0baf79591f9d8894efe922bceaf3d75b1b08055229d",
                217_254_576,
                "claude",
            ),
            (
                "x86_64-unknown-linux-gnu",
                "linux-x64",
                "1e08503dbdf3c2cb0d706d32f3408277388d1c76ef108673e8fe42c1b322925b",
                233_709_640,
                "claude",
            ),
            (
                "x86_64-unknown-linux-musl",
                "linux-x64-musl",
                "8d25ffbf600882d7b985706cc10ea45995060637ed009b5700b65e162bb00b96",
                227_467_360,
                "claude",
            ),
            (
                "x86_64-pc-windows-msvc",
                "win32-x64",
                "0e4195524b73eb77efbdf3e2b36de5322a29f0ca575dfd2d9b4f946b1d425469",
                237_100_192,
                "claude.exe",
            ),
        ] {
            let c = candidate(verify(
                spec("claude"),
                triple,
                &f,
                v("2.1.280"),
                b"2.1.280\n",
                &key,
                NOW,
            ));
            let a = c.artifact.artifact();
            assert_eq!(c.version, v("2.1.280"));
            assert_eq!(c.digest.as_str(), sha, "{triple}");
            assert_eq!(a.sha256, sha);
            assert_eq!(a.size, size);
            assert_eq!(a.url, format!("{CLAUDE}2.1.280/{platform}/{suffix}"));
            assert_eq!(a.payload, "raw-binary");
            assert_eq!(a.entry, "claude");
            let ext = if suffix.ends_with(".exe") { ".exe" } else { "" };
            assert_eq!(a.asset, format!("claude-2.1.280-{triple}{ext}"));
            assert_eq!(c.build_date, BuildDate::parse("2026-09-21T20:55:27Z"));
            admit(&c.artifact).unwrap();
        }
        // The same documents under the committed TEST key are refused: the key is the anchor.
        let test_key = crate::openpgp::testkit::test_key();
        match verify(
            spec("claude"),
            "aarch64-apple-darwin",
            &f,
            v("2.1.280"),
            b"",
            &test_key,
            NOW,
        ) {
            Resolution::Refused { reason, .. } => {
                assert!(reason.contains("not signed by Anthropic's key"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        // A document the host does not serve (a 404 while the vendor is still publishing)
        // is no verdict on any bytes…
        match verify(
            spec("claude"),
            "aarch64-apple-darwin",
            &f,
            v("2.1.281"),
            b"",
            &key,
            NOW,
        ) {
            Resolution::Unserved(why) => assert!(why.contains("404"), "{why}"),
            other => panic!("{other:?}"),
        }
        // …and a release other than the one the head named is refused by its own version.
        f.doc(&format!("{CLAUDE}2.1.281/manifest.json"), REAL_MANIFEST);
        f.doc(&format!("{CLAUDE}2.1.281/manifest.json.sig"), REAL_SIG);
        match verify(
            spec("claude"),
            "aarch64-apple-darwin",
            &f,
            v("2.1.281"),
            b"",
            &key,
            NOW,
        ) {
            Resolution::Refused { reason, .. } => {
                assert!(reason.contains("not for 2.1.281"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// THE REAL CODEX RELEASE: the JSON and OpenAI's SHA256SUMS agree on every package the
    /// publisher's rules pick — the linux glibc triple taking the static musl package.
    #[test]
    fn the_real_codex_release_resolves_when_both_hosts_agree() {
        let f = Fake::default();
        f.doc(
            &format!("{CODEX_DL}rust-v0.156.0/codex-package_SHA256SUMS"),
            REAL_SUMS,
        );
        let key = crate::openpgp::ANTHROPIC_CLAUDE_CODE_RELEASE_KEY;
        for (triple, asset, sha) in [
            (
                "aarch64-apple-darwin",
                "codex-package-aarch64-apple-darwin.tar.gz",
                "6f7bdad25693f464a146ad6f24d477ad6fbffe07b62556f829ee5d3b04f48f8b",
            ),
            (
                "x86_64-unknown-linux-gnu",
                "codex-package-x86_64-unknown-linux-musl.tar.gz",
                "e8b744b03adb90b296bf632c8a29167e75ea1b9d2980e49d3dfc6e84f5dba749",
            ),
            (
                "x86_64-pc-windows-msvc",
                "codex-package-x86_64-pc-windows-msvc.tar.gz",
                "0c17e8d90140168ff23c835d955132e1912eef1ad07be18a0bf5074e4ed137a5",
            ),
        ] {
            let url = format!("{CODEX_DL}rust-v0.156.0/{asset}");
            f.payload(&url, &[0u8; 1631]);
            let c = candidate(verify(
                spec("codex"),
                triple,
                &f,
                v("0.156.0"),
                REAL_RELEASE,
                &key,
                NOW,
            ));
            let a = c.artifact.artifact();
            assert_eq!(c.digest.as_str(), sha, "{triple}");
            assert_eq!(a.url, url);
            assert_eq!(a.size, 1631, "the exact cap is the HEAD content length");
            assert_eq!(a.payload, "tar-gz");
            assert_eq!(a.asset, format!("codex-0.156.0-{triple}.tar.gz"));
            assert_eq!(c.build_date, None);
            admit(&c.artifact).unwrap();
        }
    }

    #[test]
    fn the_heads_name_canonical_versions_only() {
        assert_eq!(claude_head_version(b"2.1.280\n"), Some(v("2.1.280")));
        assert_eq!(claude_head_version(b"2.1.280"), Some(v("2.1.280")));
        for bad in [
            &b"2.1.280\n\n"[..],
            b" 2.1.280",
            b"2.1.0280",
            b"2.1",
            b"latest",
            b"",
        ] {
            assert_eq!(claude_head_version(bad), None, "{bad:?}");
        }
        let release = strict_json(REAL_RELEASE).unwrap();
        assert_eq!(codex_tag_version(&release), Ok(v("0.156.0")));
        for bad in [
            "{\"tag_name\":\"v0.156.0\"}",
            "{\"tag_name\":\"rust-v0.156.0-alpha.1\"}",
            "{}",
        ] {
            assert!(
                codex_tag_version(&strict_json(bad.as_bytes()).unwrap()).is_err(),
                "{bad}"
            );
        }
        let f = Fake::default();
        f.doc(CODEX_HEAD, REAL_RELEASE);
        assert!(matches!(
            read_head(spec("codex"), &f, None),
            Head::Named { version, .. } if version == v("0.156.0")
        ));
        // A head answered off its pin is refused, never read.
        f.redirect(
            CODEX_HEAD,
            "https://releases.openai.com/codex/channels/other",
        );
        assert!(matches!(
            read_head(spec("codex"), &f, None),
            Head::Refused(_)
        ));
    }

    /// Strict JSON: a document whose meaning depends on which of two equal keys a parser
    /// keeps is refused — at any depth.
    #[test]
    fn a_repeated_key_is_refused_at_any_depth() {
        assert!(strict_json(b"{\"a\":1,\"b\":[{\"c\":2}]}").is_ok());
        for bad in [
            &b"{\"a\":1,\"a\":2}"[..],
            b"{\"p\":{\"x\":1,\"x\":1}}",
            b"{\"l\":[{\"k\":1,\"k\":2}]}",
            b"not json",
        ] {
            assert!(
                strict_json(bad).is_err(),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
        assert!(strict_json(REAL_MANIFEST).is_ok());
    }

    #[test]
    fn sums_are_parsed_strictly() {
        let sums = parse_sums(REAL_SUMS).unwrap();
        assert_eq!(sums.len(), 14);
        let a = "a".repeat(64);
        assert!(parse_sums(format!("{a}  x.tar.gz\n{a} *y.zip\n").as_bytes()).is_some());
        for bad in [
            format!("{a}  x\n{a}  x\n"),
            format!("{}  x\n", "a".repeat(63)),
            format!("{}g  x\n", "a".repeat(63)),
            format!("{a} x\n"),
            format!("{a}   x\n"),
            format!("{a}  ../x\n"),
            format!("{a}  x y\n"),
            format!("{a}  \n"),
        ] {
            assert!(parse_sums(bad.as_bytes()).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn the_client_triple_picks_the_platform_and_the_asset() {
        assert_eq!(
            claude_platform("aarch64-unknown-linux-gnu"),
            Some("linux-arm64")
        );
        assert_eq!(
            claude_platform("aarch64-unknown-linux-musl"),
            Some("linux-arm64-musl")
        );
        assert_eq!(claude_platform("riscv64gc-unknown-linux-gnu"), None);
        assert_eq!(
            codex_candidates("aarch64-unknown-linux-gnu"),
            [
                "codex-package-aarch64-unknown-linux-gnu.tar.gz",
                "codex-package-aarch64-unknown-linux-musl.tar.gz"
            ]
        );
        assert_eq!(
            codex_candidates("aarch64-pc-windows-msvc"),
            [
                "codex-package-aarch64-pc-windows-msvc.zip",
                "codex-package-aarch64-pc-windows-msvc.tar.gz"
            ]
        );
        assert_eq!(
            codex_candidates("x86_64-apple-darwin"),
            ["codex-package-x86_64-apple-darwin.tar.gz"]
        );
    }

    /// Admission refuses every row shape the resolver never builds.
    #[test]
    fn admission_refuses_what_the_resolver_never_builds() {
        let f = Fake::default();
        f.doc(&format!("{CLAUDE}2.1.280/manifest.json"), REAL_MANIFEST);
        f.doc(&format!("{CLAUDE}2.1.280/manifest.json.sig"), REAL_SIG);
        let key = crate::openpgp::ANTHROPIC_CLAUDE_CODE_RELEASE_KEY;
        let good = candidate(verify(
            spec("claude"),
            "aarch64-apple-darwin",
            &f,
            v("2.1.280"),
            b"",
            &key,
            NOW,
        ))
        .artifact;
        admit(&good).unwrap();
        type Tweak = Box<dyn Fn(&mut Artifact)>;
        let tweaks: Vec<Tweak> = vec![
            Box::new(|a| a.url = "https://evil.example/claude".into()),
            Box::new(|a| a.url = format!("{CLAUDE}2.1.280/../x/claude")),
            Box::new(|a| a.payload = "tar-gz".into()),
            Box::new(|a| a.entry = "sh".into()),
            Box::new(|a| {
                a.links.insert("x".into(), "y".into());
            }),
            Box::new(|a| a.tree_root = "0".repeat(64)),
            Box::new(|a| a.strip_components = 1),
            Box::new(|a| a.size = 0),
            Box::new(|a| a.sha256 = "A".repeat(64)),
            Box::new(|a| a.asset = "claude-2.1.279-aarch64-apple-darwin".into()),
            Box::new(|a| a.protocol = "github-release".into()),
        ];
        for (i, tweak) in tweaks.iter().enumerate() {
            let mut row = good.clone();
            tweak(&mut row.artifact);
            assert!(admit(&row).is_err(), "tweak {i} was admitted");
        }
        let mut other = good;
        other.program = "codex";
        assert!(admit(&other).is_err(), "a claude row is not codex's");
    }
}
