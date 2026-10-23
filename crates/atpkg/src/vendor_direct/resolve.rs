// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Vendor-direct head and digest resolution authenticated by the program's anchor.

use std::collections::{BTreeMap, BTreeSet};

use super::digests::AuthenticatedDigest;
use super::table::{Anchor, VendorSpec};
use super::version::{BuildDate, Version};
use crate::flow::{Fetcher, FlowError, VendorFetchError, VendorGet};
use crate::manifest::{Artifact, Cost, DigestAlg};
use crate::openpgp::PinnedRsaKey;

const CLAUDE_HEAD_CAP: u64 = 64;
const CODEX_HEAD_CAP: u64 = 1024 * 1024;
const SMALL_JSON_CAP: u64 = 4 * 1024;
const MANIFEST_CAP: u64 = 64 * 1024;
const SUMS_CAP: u64 = 64 * 1024;
const ARCHIVE_UNPACK_RATIO: u64 = 4;
const CODEX_DOWNLOADS: &str = "https://github.com/openai/codex/releases/download/";
const AGY_BUCKET: &str = "https://storage.googleapis.com/antigravity-public/antigravity-cli/";
const MUSE_DOWNLOAD: &str = "https://lookaside.facebook.com/lookaside/muse/download/";

/// What the head said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Head {
    /// 304 to the conditional GET.
    Unchanged {
        /// The validator to keep.
        etag: String,
    },
    /// The host was not reached, or is not serving now.
    Unreachable(String),
    /// The host answered, but not with the head.
    Unserved(String),
    /// The head could not be read as a canonical version in its program's grammar, or landed
    /// off its pin.
    Refused(String),
    /// The head names `version`.
    Named {
        /// The named version.
        version: Version,
        /// The response's validator.
        etag: Option<String>,
        /// The document.
        body: Vec<u8>,
    },
}

/// A payload row the vendor lane built from authenticated documents ([`admit`]).
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
    /// The payload's authenticated digest.
    pub digest: AuthenticatedDigest,
}

/// What verifying a head's documents came to.
#[derive(Debug, Clone)]
pub(crate) enum Resolution {
    /// A vendor host was not reached.
    Unreachable,
    /// A vendor host answered, but not with the document.
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

/// A vendor document GET in [`Fetcher::vendor_get`]'s shape.
pub type VendorGetFn<'a> =
    dyn Fn(&str, &str, u64, Option<&str>) -> Result<VendorGet, VendorFetchError> + 'a;

/// GET `spec`'s head ([`Fetcher::vendor_head`]).
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

/// The head's document asked again, unconditionally and patiently ([`Fetcher::vendor_get`]).
pub(crate) fn reread_head(spec: &VendorSpec, fetcher: &dyn Fetcher) -> Head {
    read_head_via(
        spec,
        &|program, url, cap, etag| fetcher.vendor_get(program, url, cap, etag),
        None,
    )
}

/// [`read_head`] over a bare GET.
pub(crate) fn read_head_via(
    spec: &VendorSpec,
    get: &VendorGetFn<'_>,
    if_none_match: Option<&str>,
) -> Head {
    let cap = match spec.anchor {
        Anchor::AnthropicOpenPgp => CLAUDE_HEAD_CAP,
        Anchor::OpenAiTwoHost => CODEX_HEAD_CAP,
        Anchor::GoogleTwoHost | Anchor::MetaDeveloperId => SMALL_JSON_CAP,
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
    match head_version(spec, &body) {
        Ok(version) if version.has_revision() == spec.revision_versions => Head::Named {
            version,
            etag,
            body,
        },
        Ok(version) => Head::Refused(format!(
            "the release channel's head names {version}, not a version in {}'s grammar",
            spec.program
        )),
        Err(why) => Head::Refused(why),
    }
}

fn head_version(spec: &VendorSpec, body: &[u8]) -> Result<Version, String> {
    let canonical = || String::from("the release channel's head names no canonical version");
    match spec.anchor {
        Anchor::AnthropicOpenPgp => claude_head_version(body).ok_or_else(canonical),
        Anchor::OpenAiTwoHost => strict_json(body)
            .ok()
            .and_then(|doc| codex_tag_version(&doc).ok())
            .ok_or_else(canonical),
        Anchor::GoogleTwoHost => agy_manifest(body)
            .and_then(|m| Version::parse(&m.version))
            .ok_or_else(canonical),
        Anchor::MetaDeveloperId => muse_head_version(spec, body),
    }
}

/// Verify the documents behind `version`.
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
        Anchor::GoogleTwoHost => verify_agy(spec, triple, fetcher, version, head_body),
        Anchor::MetaDeveloperId => verify_muse(spec, triple, fetcher, version),
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

/// Whether `anchor`'s vendor publishes a build the lane can verify for `triple`
/// ([`VendorSpec::serves`]).
pub(crate) fn serves(anchor: Anchor, triple: &str) -> bool {
    match anchor {
        Anchor::AnthropicOpenPgp => claude_platform(triple).is_some(),
        Anchor::OpenAiTwoHost => true,
        Anchor::GoogleTwoHost => agy_platform(triple).is_some(),
        Anchor::MetaDeveloperId => muse_platform(triple).is_some(),
    }
}

/// Admits a synthetic row before any byte moves.
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
    let exposed = spec.exposes.contains(&a.entry.as_str());
    let lane_ok = match spec.anchor {
        Anchor::OpenAiTwoHost => {
            matches!(a.payload.as_str(), "tar-gz" | "zip") && a.entry.is_empty()
        }
        anchor => a.payload == payload_of(anchor, &a.target) && exposed,
    };
    if !lane_ok {
        return refuse("payload lane or entry is not the program's");
    }
    if a.strip_components != 0 || !a.links.is_empty() || !a.tree_root.is_empty() {
        return refuse("a vendor-direct row carries no strip, links or signed root");
    }
    let alg = spec.anchor.digest_alg();
    if a.digest()
        .is_none_or(|(got, hex)| got != alg || !alg.is_lower_hex(hex))
        || a.size == 0
    {
        return refuse("a vendor-direct row needs its anchor's one digest and an exact size");
    }
    if a.asset != asset_name(row.program, row.version, &a.target, &a.payload) {
        return refuse("asset is not the per-version local name");
    }
    Ok(())
}

/// The staging name `spec`'s payload of `version` downloads to on `triple` ([`asset_name`],
/// [`payload_of`]).
pub(crate) fn staging_asset(spec: &VendorSpec, version: Version, triple: &str) -> String {
    asset_name(
        spec.program,
        version,
        triple,
        payload_of(spec.anchor, triple),
    )
}

fn payload_of(anchor: Anchor, triple: &str) -> &'static str {
    let windows = triple.contains("-windows-");
    match anchor {
        Anchor::AnthropicOpenPgp | Anchor::MetaDeveloperId => "raw-binary",
        Anchor::GoogleTwoHost if windows => "raw-binary",
        Anchor::GoogleTwoHost => "tar-gz-binary",
        Anchor::OpenAiTwoHost if windows => "zip",
        Anchor::OpenAiTwoHost => "tar-gz",
    }
}

fn asset_name(program: &str, version: Version, triple: &str, payload: &str) -> String {
    let ext = match payload {
        "tar-gz" | "tar-gz-binary" => ".tar.gz",
        "zip" => ".zip",
        _ if triple.contains("-windows-") => ".exe",
        _ => "",
    };
    format!("{program}-{version}-{triple}{ext}")
}

enum Fault {
    Unreachable,
    Unserved(String),
    Refused(String),
}

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

fn claude_head_version(body: &[u8]) -> Option<Version> {
    let text = std::str::from_utf8(body).ok()?;
    Version::parse(text.strip_suffix('\n').unwrap_or(text))
}

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
    let Some(digest) =
        checksum.and_then(|c| AuthenticatedDigest::authenticated(DigestAlg::Sha256, c))
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
    let payload = payload_of(spec.anchor, triple);
    let row = vouched(spec, version, triple, &url, digest, size, payload);
    Ok(Candidate { build_date, ..row })
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
    let Some(digest) = AuthenticatedDigest::authenticated(DigestAlg::Sha256, &from_sums) else {
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
    Ok(vouched(spec, version, triple, &url, digest, size, payload))
}

/// agy's platform for the client's own triple: `(Run manifest key, bucket directory, payload
/// file)`, as agy's own updater and Google's bucket spell them.
pub(super) fn agy_platform(triple: &str) -> Option<(&'static str, &'static str, &'static str)> {
    Some(match triple {
        "aarch64-apple-darwin" => ("darwin_arm64", "darwin-arm", "cli_mac_arm64.tar.gz"),
        "x86_64-apple-darwin" => ("darwin_amd64", "darwin-x64", "cli_mac_x64.tar.gz"),
        "x86_64-unknown-linux-gnu" => ("linux_amd64", "linux-x64", "cli_linux_x64.tar.gz"),
        "aarch64-unknown-linux-gnu" => ("linux_arm64", "linux-arm", "cli_linux_arm64.tar.gz"),
        "x86_64-unknown-linux-musl" => (
            "linux_amd64_musl",
            "linux-x64-musl",
            "cli_linux_x64_musl.tar.gz",
        ),
        "aarch64-unknown-linux-musl" => (
            "linux_arm64_musl",
            "linux-arm-musl",
            "cli_linux_arm64_musl.tar.gz",
        ),
        "x86_64-pc-windows-msvc" => ("windows_amd64", "windows-x64", "cli_windows_x64.exe"),
        "aarch64-pc-windows-msvc" => ("windows_arm64", "windows-arm", "cli_windows_arm64.exe"),
        _ => return None,
    })
}

/// One agy manifest.
#[derive(Debug, PartialEq, Eq)]
struct AgyManifest {
    version: String,
    url: String,
    sha512: String,
}

fn agy_manifest(body: &[u8]) -> Option<AgyManifest> {
    let doc = strict_json(body).ok()?;
    let field = |key: &str| doc.get(key)?.as_str().map(str::to_string);
    Some(AgyManifest {
        version: field("version")?,
        url: field("url")?,
        sha512: field("sha512")?,
    })
}

fn agy_release_dir<'u>(url: &'u str, version: Version, triple: &str) -> Option<&'u str> {
    let (_, dir, file) = agy_platform(triple)?;
    let rest = url.strip_prefix(AGY_BUCKET)?;
    let mut parts = rest.split('/');
    let (release, d, f) = (parts.next()?, parts.next()?, parts.next()?);
    let execution = release.strip_prefix(&format!("{version}-"))?;
    let execution_ok =
        (1..=20).contains(&execution.len()) && execution.bytes().all(|b| b.is_ascii_digit());
    (parts.next().is_none() && execution_ok && d == dir && f == file)
        .then(|| &url[..url.len() - file.len()])
}

fn verify_agy(
    spec: &'static VendorSpec,
    triple: &str,
    fetcher: &dyn Fetcher,
    version: Version,
    run_manifest: &[u8],
) -> Result<Candidate, Fault> {
    if agy_platform(triple).is_none() {
        return refused(format!("Google publishes no agy build for {triple}"));
    }
    let Some(head) = agy_manifest(run_manifest) else {
        return refused("the Run manifest is not strict JSON naming a version, url and sha512");
    };
    if head.version != version.to_string() {
        return refused(format!("the Run manifest is not for {version}"));
    }
    let Some(dir) = agy_release_dir(&head.url, version, triple) else {
        return refused(format!(
            "{} is not {AGY_BUCKET}{version}-<execution id>/ under {triple}'s directory and file",
            head.url
        ));
    };
    let beside_url = format!("{dir}manifest.json");
    let beside = digest_doc(spec, fetcher, &beside_url, SMALL_JSON_CAP)?;
    if agy_manifest(&beside).as_ref() != Some(&head) {
        return refused(format!(
            "the Run manifest and {beside_url} disagree on the version, url or sha512"
        ));
    }
    let alg = spec.anchor.digest_alg();
    let Some(digest) = AuthenticatedDigest::authenticated(alg, &head.sha512) else {
        return refused(format!("the manifests' {alg} is not 128 hex digits"));
    };
    let size = fetcher.vendor_content_length(spec.program, &head.url)?;
    if size == 0 {
        return refused(format!("{} answered no content length", head.url));
    }
    let payload = payload_of(spec.anchor, triple);
    Ok(vouched(
        spec, version, triple, &head.url, digest, size, payload,
    ))
}

/// muse's release-manifest key and payload file for `triple`: darwin only.
pub(super) fn muse_platform(triple: &str) -> Option<(&'static str, &'static str)> {
    Some(match triple {
        "aarch64-apple-darwin" => ("aarch64_macos", "muse-aarch64-macos"),
        "x86_64-apple-darwin" => ("x86_macos", "muse-x86-macos"),
        _ => return None,
    })
}

/// The lookaside URL of `file` in muse's release `version`, built by the client.
pub(super) fn muse_url(version: Version, file: &str) -> String {
    format!("{MUSE_DOWNLOAD}?channel=muse&version={version}&file={file}")
}

fn muse_head_version(spec: &VendorSpec, body: &[u8]) -> Result<Version, String> {
    let doc = strict_json(body).map_err(|()| String::from("muse's channel is not strict JSON"))?;
    let field = |key: &str| doc.get(key).and_then(aterm_json::Value::as_str);
    let channel = spec.head_url.rsplit('/').next().unwrap_or_default();
    if field("channel") != Some(channel) {
        return Err(format!("muse's channel does not name itself {channel}"));
    }
    let version = field("version")
        .and_then(Version::parse)
        .ok_or_else(|| String::from("muse's channel does not name a canonical version"))?;
    let expected = muse_url(version, "manifest.json");
    if field("manifest_url") != Some(expected.as_str()) {
        return Err(format!(
            "muse's channel names a manifest other than {expected}"
        ));
    }
    Ok(version)
}

fn verify_muse(
    spec: &'static VendorSpec,
    triple: &str,
    fetcher: &dyn Fetcher,
    version: Version,
) -> Result<Candidate, Fault> {
    let Some((platform, file)) = muse_platform(triple) else {
        return refused(format!(
            "Meta publishes no muse build aterm can verify for {triple}"
        ));
    };
    let manifest_url = muse_url(version, "manifest.json");
    let manifest = digest_doc(spec, fetcher, &manifest_url, MANIFEST_CAP)?;
    let Ok(doc) = strict_json(&manifest) else {
        return refused(
            "muse's release manifest is not strict JSON (a key repeats or it does not parse)",
        );
    };
    if doc.get("version").and_then(aterm_json::Value::as_str) != Some(version.to_string().as_str())
    {
        return refused(format!("muse's release manifest is not for {version}"));
    }
    let alg = spec.anchor.digest_alg();
    if doc
        .get("checksum_algorithm")
        .and_then(aterm_json::Value::as_str)
        != Some(alg.key())
    {
        return refused(format!(
            "muse's release manifest does not sign {alg} checksums"
        ));
    }
    let Some(row) = doc.pointer(&format!("/artifacts/{platform}")) else {
        return refused(format!("muse's release manifest names no {platform} build"));
    };
    let url = muse_url(version, file);
    if row.get("url").and_then(aterm_json::Value::as_str) != Some(url.as_str()) {
        return refused(format!(
            "muse's release manifest names {platform} somewhere other than {url}"
        ));
    }
    let checksum = row.get("checksum").and_then(aterm_json::Value::as_str);
    let Some(digest) = checksum.and_then(|c| AuthenticatedDigest::authenticated(alg, c)) else {
        return refused(format!(
            "muse's release manifest carries no {alg} for {platform}"
        ));
    };
    let Some(size) = row
        .get("size")
        .and_then(aterm_json::Value::as_u64)
        .filter(|&s| s > 0)
    else {
        return refused(format!(
            "muse's release manifest carries no size for {platform}"
        ));
    };
    let payload = payload_of(spec.anchor, triple);
    Ok(vouched(spec, version, triple, &url, digest, size, payload))
}

fn vouched(
    spec: &'static VendorSpec,
    version: Version,
    triple: &str,
    url: &str,
    digest: AuthenticatedDigest,
    size: u64,
    payload: &str,
) -> Candidate {
    let entry = if crate::vendor::is_archive_payload(payload) {
        ""
    } else {
        spec.exposes.first().copied().unwrap_or(spec.program)
    };
    let disk_installed = if payload == "raw-binary" {
        size
    } else {
        size.saturating_mul(ARCHIVE_UNPACK_RATIO)
    };
    let (sha256, sha512) = digest.keyed();
    let artifact = SyntheticArtifact {
        program: spec.program,
        version,
        artifact: Artifact {
            target: triple.to_string(),
            kind: "binary".into(),
            protocol: "https".into(),
            asset: asset_name(spec.program, version, triple, payload),
            sha256,
            sha512,
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
            provenance: None,
        },
    };
    Candidate {
        version,
        artifact,
        build_date: None,
        anchor: spec.anchor,
        digest,
    }
}

fn codex_tag_version(doc: &aterm_json::Value) -> Result<Version, ()> {
    doc.get("tag_name")
        .and_then(aterm_json::Value::as_str)
        .and_then(|t| t.strip_prefix("rust-v"))
        .and_then(Version::parse)
        .ok_or(())
}

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

fn parse_sums(text: &[u8]) -> Option<BTreeMap<String, String>> {
    let text = std::str::from_utf8(text).ok()?;
    let mut out = BTreeMap::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let (digest, rest) = (line.get(..64)?, line.get(64..)?);
        if !aterm_codec::hex::is_hex(digest, 64) {
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
        .filter(|d| aterm_codec::hex::is_hex(d, 64))
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| format!("the release JSON carries no sha256 digest for {name}"))
}

fn strict_json(bytes: &[u8]) -> Result<aterm_json::Value, ()> {
    aterm_json::from_slice::<Strict>(bytes)
        .map(|s| s.0)
        .map_err(|_| ())
}

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
    const AGY_DARWIN_ARM: &[u8] = include_bytes!("fixtures/agy-1.3.6-darwin_arm64.json");
    const AGY_BESIDE_DARWIN_ARM: &[u8] =
        include_bytes!("fixtures/agy-1.3.6-darwin-arm-manifest.json");
    const AGY_LINUX_X64: &[u8] = include_bytes!("fixtures/agy-1.3.6-linux_amd64.json");
    const AGY_WINDOWS_X64: &[u8] = include_bytes!("fixtures/agy-1.3.6-windows_amd64.json");
    const MUSE_CHANNEL: &[u8] = include_bytes!("fixtures/muse-stable-channel.json");
    const MUSE_MANIFEST: &[u8] = include_bytes!("fixtures/muse-1.4.9-R6297.1-manifest.json");
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
        f.redirect(
            CODEX_HEAD,
            "https://releases.openai.com/codex/channels/other",
        );
        assert!(matches!(
            read_head(spec("codex"), &f, None),
            Head::Refused(_)
        ));
    }

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
            Box::new(|a| a.sha512 = "5".repeat(128)),
            Box::new(|a| {
                a.sha256 = String::new();
                a.sha512 = "5".repeat(64);
            }),
            Box::new(|a| {
                a.sha256 = String::new();
                a.sha512 = "A".repeat(128);
            }),
            Box::new(|a| a.sha256 = String::new()),
            Box::new(|a| a.asset = "claude-2.1.279-aarch64-apple-darwin".into()),
            Box::new(|a| a.protocol = "github-release".into()),
        ];
        for (i, tweak) in tweaks.iter().enumerate() {
            let mut row = good.clone();
            tweak(&mut row.artifact);
            assert!(admit(&row).is_err(), "tweak {i} was admitted");
        }
        let a = good.artifact();
        let sha512 = AuthenticatedDigest::authenticated(DigestAlg::Sha512, &"5".repeat(128));
        let by_512 = vouched(
            spec("claude"),
            good.version,
            &a.target,
            &a.url,
            sha512.unwrap(),
            a.size,
            &a.payload,
        )
        .artifact;
        assert_eq!(
            by_512.artifact().digest(),
            Some((DigestAlg::Sha512, "5".repeat(128).as_str()))
        );
        assert!(by_512.artifact().sha256.is_empty());
        assert!(
            admit(&by_512).is_err(),
            "claude's anchor vouches for a sha256"
        );
        let mut other = good;
        other.program = "codex";
        assert!(admit(&other).is_err(), "a claude row is not codex's");
    }

    fn refusal(r: Resolution) -> String {
        match r {
            Resolution::Refused { reason, .. } => reason,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    fn edit(doc: &[u8], from: &str, to: &str) -> Vec<u8> {
        let text = std::str::from_utf8(doc).unwrap();
        assert!(text.contains(from), "{from}");
        text.replacen(from, to, 1).into_bytes()
    }

    const AGY_RELEASE: &str =
        "https://storage.googleapis.com/antigravity-public/antigravity-cli/1.3.6-5068262057312256/";

    fn verify_agy_on(f: &Fake, triple: &str, version: &str, head: &[u8]) -> Resolution {
        let key = crate::openpgp::ANTHROPIC_CLAUDE_CODE_RELEASE_KEY;
        verify(spec("agy"), triple, f, v(version), head, &key, NOW)
    }

    #[test]
    fn the_real_agy_release_resolves_when_both_google_hosts_agree() {
        let f = Fake::default();
        for (triple, head, beside, dir_file, payload, ext, ratio) in [
            (
                "aarch64-apple-darwin",
                AGY_DARWIN_ARM,
                AGY_BESIDE_DARWIN_ARM,
                "darwin-arm/cli_mac_arm64.tar.gz",
                "tar-gz-binary",
                ".tar.gz",
                ARCHIVE_UNPACK_RATIO,
            ),
            (
                "x86_64-unknown-linux-gnu",
                AGY_LINUX_X64,
                AGY_LINUX_X64,
                "linux-x64/cli_linux_x64.tar.gz",
                "tar-gz-binary",
                ".tar.gz",
                ARCHIVE_UNPACK_RATIO,
            ),
            (
                "x86_64-pc-windows-msvc",
                AGY_WINDOWS_X64,
                AGY_WINDOWS_X64,
                "windows-x64/cli_windows_x64.exe",
                "raw-binary",
                ".exe",
                1,
            ),
        ] {
            let url = format!("{AGY_RELEASE}{dir_file}");
            let dir = &url[..url.rfind('/').unwrap() + 1];
            f.doc(&format!("{dir}manifest.json"), beside);
            f.payload(&url, &[0u8; 1234]);
            let c = candidate(verify_agy_on(&f, triple, "1.3.6", head));
            let a = c.artifact.artifact();
            let sha512 = agy_manifest(head).unwrap().sha512;
            assert_eq!(c.digest.alg(), DigestAlg::Sha512, "{triple}");
            assert_eq!(c.digest.as_str(), sha512);
            assert_eq!(
                (a.sha256.as_str(), a.sha512.as_str()),
                ("", sha512.as_str())
            );
            assert_eq!(a.url, url);
            assert_eq!(a.size, 1234, "the exact cap is the HEAD content length");
            assert_eq!((a.payload.as_str(), a.entry.as_str()), (payload, "agy"));
            assert_eq!(a.cost.disk_installed, 1234 * ratio);
            assert_eq!(a.asset, format!("agy-1.3.6-{triple}{ext}"));
            assert_eq!((c.build_date, c.anchor), (None, Anchor::GoogleTwoHost));
            admit(&c.artifact).unwrap();
            let mut by_256 = c.artifact.clone();
            by_256.artifact.sha512 = String::new();
            by_256.artifact.sha256 = "a".repeat(64);
            assert!(admit(&by_256).is_err(), "{triple}");
        }
        f.doc(spec("agy").head_url, AGY_DARWIN_ARM);
        assert!(matches!(
            read_head(spec("agy"), &f, None),
            Head::Named { version, .. } if version == v("1.3.6")
        ));
    }

    #[test]
    fn agy_refuses_two_hosts_that_do_not_vouch_for_one_payload() {
        const TRIPLE: &str = "aarch64-apple-darwin";
        let beside_url = format!("{AGY_RELEASE}darwin-arm/manifest.json");
        let payload_url = format!("{AGY_RELEASE}darwin-arm/cli_mac_arm64.tar.gz");
        let real = agy_manifest(AGY_DARWIN_ARM).unwrap();
        let f = Fake::default();
        f.payload(&payload_url, &[0u8; 7]);
        for disagreeing in [
            edit(
                AGY_BESIDE_DARWIN_ARM,
                "cli_mac_arm64.tar.gz",
                "cli_mac_x64.tar.gz",
            ),
            edit(AGY_BESIDE_DARWIN_ARM, &real.sha512[..8], "00000000"),
            edit(
                AGY_BESIDE_DARWIN_ARM,
                "\"version\": \"1.3.6\"",
                "\"version\": \"1.3.5\"",
            ),
        ] {
            f.doc(&beside_url, &disagreeing);
            let why = refusal(verify_agy_on(&f, TRIPLE, "1.3.6", AGY_DARWIN_ARM));
            assert!(why.contains("disagree"), "{why}");
        }
        f.doc(&beside_url, AGY_BESIDE_DARWIN_ARM);
        candidate(verify_agy_on(&f, TRIPLE, "1.3.6", AGY_DARWIN_ARM));
        for (from, to) in [
            (
                "antigravity-public/antigravity-cli/",
                "antigravity-public/other/",
            ),
            ("darwin-arm/", "linux-x64/"),
            ("cli_mac_arm64.tar.gz", "cli_mac_arm64.zip"),
            ("1.3.6-5068262057312256", "1.3.5-5068262057312256"),
            ("1.3.6-5068262057312256", "1.3.6-"),
            ("1.3.6-5068262057312256", "1.3.6-506826205731225612345"),
            ("1.3.6-5068262057312256", "1.3.6-50682620573x2256"),
            ("darwin-arm/cli", "darwin-arm/x/cli"),
        ] {
            let why = refusal(verify_agy_on(
                &f,
                TRIPLE,
                "1.3.6",
                &edit(AGY_DARWIN_ARM, from, to),
            ));
            assert!(why.contains(" is not "), "{to}: {why}");
        }
        let why = refusal(verify_agy_on(
            &f,
            "x86_64-unknown-linux-gnu",
            "1.3.6",
            AGY_DARWIN_ARM,
        ));
        assert!(why.contains(" is not "), "{why}");
        let why = refusal(verify_agy_on(&f, TRIPLE, "1.3.7", AGY_DARWIN_ARM));
        assert!(why.contains("not for 1.3.7"), "{why}");
        let short = format!("\"sha512\": \"{}\"", &real.sha512[..127]);
        let full = format!("\"sha512\": \"{}\"", real.sha512);
        let truncated = edit(AGY_DARWIN_ARM, &full, &short);
        f.doc(&beside_url, &truncated);
        let why = refusal(verify_agy_on(&f, TRIPLE, "1.3.6", &truncated));
        assert!(why.contains("not 128 hex"), "{why}");
        let doubled = edit(AGY_DARWIN_ARM, "{", "{\"version\": \"1.3.6\",");
        let why = refusal(verify_agy_on(&f, TRIPLE, "1.3.6", &doubled));
        assert!(why.contains("strict JSON"), "{why}");
        f.doc(&beside_url, AGY_BESIDE_DARWIN_ARM);
        f.redirect(
            &beside_url,
            &format!("{AGY_RELEASE}darwin-x64/manifest.json"),
        );
        let why = refusal(verify_agy_on(&f, TRIPLE, "1.3.6", AGY_DARWIN_ARM));
        assert!(why.contains("off its pinned host"), "{why}");
        f.forget(&beside_url);
        assert!(matches!(
            verify_agy_on(&f, TRIPLE, "1.3.6", AGY_DARWIN_ARM),
            Resolution::Unserved(_)
        ));
        let head = spec("agy").head_url;
        f.doc(head, AGY_DARWIN_ARM);
        f.redirect(head, &format!("{AGY_RELEASE}darwin-arm/manifest.json"));
        assert!(matches!(read_head(spec("agy"), &f, None), Head::Refused(_)));
    }

    #[test]
    fn the_real_muse_release_resolves_on_darwin_only() {
        let f = Fake::default();
        let release = v("1.4.9-R6297.1");
        f.doc(spec("muse").head_url, MUSE_CHANNEL);
        assert!(matches!(
            read_head(spec("muse"), &f, None),
            Head::Named { version, .. } if version == release
        ));
        f.doc(&muse_url(release, "manifest.json"), MUSE_MANIFEST);
        let key = crate::openpgp::ANTHROPIC_CLAUDE_CODE_RELEASE_KEY;
        for (triple, file, sha, size) in [
            (
                "aarch64-apple-darwin",
                "muse-aarch64-macos",
                "2b856618ae3de3a63956e6d510b849ad707b16e5d2ed196299f3fe5ef2e3ff5e",
                368_615_952,
            ),
            (
                "x86_64-apple-darwin",
                "muse-x86-macos",
                "4c6ed04ccd5a453bd7b38799aeb9eb290b8635b32c95b5a35e6950a2f20446cb",
                389_577_808,
            ),
        ] {
            let c = candidate(verify(
                spec("muse"),
                triple,
                &f,
                release,
                MUSE_CHANNEL,
                &key,
                NOW,
            ));
            let a = c.artifact.artifact();
            assert_eq!(
                (c.digest.alg(), c.digest.as_str()),
                (DigestAlg::Sha256, sha)
            );
            assert_eq!((a.sha256.as_str(), a.size), (sha, size));
            assert_eq!(
                a.url,
                format!(
                    "https://lookaside.facebook.com/lookaside/muse/download/?channel=muse\
                     &version=1.4.9-R6297.1&file={file}"
                )
            );
            assert_eq!(
                (a.payload.as_str(), a.entry.as_str()),
                ("raw-binary", "muse")
            );
            assert_eq!(a.asset, format!("muse-1.4.9-R6297.1-{triple}"));
            assert_eq!(c.anchor, Anchor::MetaDeveloperId);
            admit(&c.artifact).unwrap();
        }
        let asked = f.gets.borrow().len();
        for triple in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
            assert!(!spec("muse").serves(triple), "{triple}");
            let why = refusal(verify(
                spec("muse"),
                triple,
                &f,
                release,
                MUSE_CHANNEL,
                &key,
                NOW,
            ));
            assert!(why.contains("no muse build aterm can verify"), "{why}");
        }
        assert_eq!(f.gets.borrow().len(), asked, "nothing was asked");
    }

    #[test]
    fn muse_refuses_every_document_that_is_not_the_client_built_one() {
        let f = Fake::default();
        let head = spec("muse").head_url;
        for bad in [
            edit(MUSE_CHANNEL, "file=manifest.json", "file=manifest2.json"),
            edit(
                MUSE_CHANNEL,
                "lookaside.facebook.com",
                "lookaside.example.com",
            ),
            edit(
                MUSE_CHANNEL,
                "\"channel\":\"muse-stable\"",
                "\"channel\":\"muse-beta\"",
            ),
        ] {
            f.doc(head, &bad);
            assert!(
                matches!(read_head(spec("muse"), &f, None), Head::Refused(_)),
                "{}",
                String::from_utf8_lossy(&bad)
            );
        }
        let release = v("1.4.9-R6297.1");
        let manifest_url = muse_url(release, "manifest.json");
        let key = crate::openpgp::ANTHROPIC_CLAUDE_CODE_RELEASE_KEY;
        let muse = |f: &Fake| {
            verify(
                spec("muse"),
                "aarch64-apple-darwin",
                f,
                release,
                b"",
                &key,
                NOW,
            )
        };
        for (from, to, said) in [
            (
                "\"checksum_algorithm\":\"sha256\"",
                "\"checksum_algorithm\":\"md5\"",
                "sha256",
            ),
            (
                "file=muse-aarch64-macos\"",
                "file=muse-aarch64-macos2\"",
                "somewhere other than",
            ),
            ("\"size\":368615952", "\"size\":0", "no size"),
            (
                "\"version\":\"1.4.9-R6297.1\"",
                "\"version\":\"1.4.9-R6297.2\"",
                "not for 1.4.9-R6297.1",
            ),
            ("2b856618", "2b85661", "no sha256"),
        ] {
            f.doc(&manifest_url, &edit(MUSE_MANIFEST, from, to));
            let why = refusal(muse(&f));
            assert!(why.contains(said), "{to}: {why}");
        }
        f.doc(&manifest_url, MUSE_MANIFEST);
        candidate(muse(&f));
        f.redirect(&manifest_url, &muse_url(release, "manifest2.json"));
        let why = refusal(muse(&f));
        assert!(why.contains("off its pinned host"), "{why}");
    }

    #[test]
    fn a_head_in_the_other_grammar_is_refused() {
        let f = Fake::default();
        let plain = format!(
            "{{\"channel\":\"muse-stable\",\"version\":\"1.5.0\",\"manifest_url\":\"{}\"}}",
            muse_url(v("1.5.0"), "manifest.json")
        );
        f.doc(spec("muse").head_url, plain.as_bytes());
        match read_head(spec("muse"), &f, None) {
            Head::Refused(why) => assert!(why.contains("muse's grammar"), "{why}"),
            other => panic!("{other:?}"),
        }
        let claude_head = format!("{CLAUDE}latest");
        f.doc(&claude_head, b"2.1.281-R5\n");
        match read_head(spec("claude"), &f, None) {
            Head::Refused(why) => assert!(why.contains("claude's grammar"), "{why}"),
            other => panic!("{other:?}"),
        }
        f.doc(CODEX_HEAD, br#"{"tag_name":"rust-v0.156.0-R1"}"#);
        assert!(matches!(
            read_head(spec("codex"), &f, None),
            Head::Refused(_)
        ));
        f.doc(&claude_head, b"2.1.281\n");
        assert!(matches!(
            read_head(spec("claude"), &f, None),
            Head::Named { .. }
        ));
    }
}
