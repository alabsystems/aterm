// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Vendor-direct head and digest resolution authenticated by the program's anchor.

use std::collections::{BTreeMap, BTreeSet};

use super::digests::AuthenticatedDigest;
use super::npm::NpmKey;
use super::table::{Anchor, VendorSpec};
use super::version::{BuildDate, Version};
use crate::flow::{Fetcher, FlowError, VendorFetchError, VendorGet};
use crate::manifest::{Artifact, Cost, DigestAlg};
use crate::openpgp::PinnedRsaKey;

const PLAIN_HEAD_CAP: u64 = 64;
const CODEX_HEAD_CAP: u64 = 1024 * 1024;
const SMALL_JSON_CAP: u64 = 4 * 1024;
const MANIFEST_CAP: u64 = 64 * 1024;
const SUMS_CAP: u64 = 64 * 1024;
const ARCHIVE_UNPACK_RATIO: u64 = 4;
const CODEX_DOWNLOADS: &str = "https://github.com/openai/codex/releases/download/";
const AGY_BUCKET: &str = "https://storage.googleapis.com/antigravity-public/antigravity-cli/";
const MUSE_DOWNLOAD: &str = "https://lookaside.facebook.com/lookaside/muse/download/";
const KIMI_CDN: &str = "https://cdn.kimi.com/kimi-code/";
const KIMI_GITHUB: &str = "https://github.com/MoonshotAI/kimi-code/releases/download/";
const GROK_BUCKET: &str = "https://storage.googleapis.com/grok-build-public-artifacts/cli/";
const NPM_REGISTRY: &str = "https://registry.npmjs.org/";
/// The largest grok binary the lane stages: the bucket's raw object size is the exact length
/// the npm tarball's `bin/grok.br` must decode to, and no document signs that length.
const GROK_MAX_BINARY: u64 = 1 << 30;
/// The largest npm tarball or bucket `.zst` of grok the lane downloads (1.0.70's are 50 to 58 MB).
const GROK_MAX_DOWNLOAD: u64 = 256 << 20;

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
    /// What vouches for the payload's digest.
    pub vouch: Vouch,
    /// The vendor's own copy of the binary the staged one must agree with (grok's bucket).
    pub companion: Option<Companion>,
}

/// What vouches for a candidate's payload digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Vouch {
    /// The anchor's documents named it before any byte moved.
    Documents(AuthenticatedDigest),
}

impl Vouch {
    /// The digest the documents named.
    #[must_use]
    pub(crate) const fn digest(&self) -> &AuthenticatedDigest {
        match self {
            Self::Documents(d) => d,
        }
    }
}

/// xAI's bucket copy of a grok build, which the binary decoded from npm's signed tarball must
/// equal byte for byte ([`super::lane`]'s agreement step).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Companion {
    /// The bucket's zstd frame of the binary (`grok-<v>-<platform>.zst`).
    pub url: String,
    /// Its exact size, from the bucket.
    pub size: u64,
    /// The binary's exact size: the bucket's raw object's, which the frame must declare and
    /// the tarball's `bin/grok.br` must decode to.
    pub binary_size: u64,
    /// The npm package the tarball is, for its own `package.json` to name.
    pub platform: GrokPlatform,
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
        Anchor::AnthropicOpenPgp
        | Anchor::MoonshotTwoHost
        | Anchor::XaiDeveloperId
        | Anchor::XaiNpmRegistry => PLAIN_HEAD_CAP,
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
        Anchor::AnthropicOpenPgp
        | Anchor::MoonshotTwoHost
        | Anchor::XaiDeveloperId
        | Anchor::XaiNpmRegistry => plain_head_version(body).ok_or_else(canonical),
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

/// The keys the anchors' signatures are held to.
#[derive(Clone, Copy)]
pub(crate) struct Keys<'a> {
    /// Anthropic's release key: claude's digest anchor.
    pub claude: &'a PinnedRsaKey,
    /// npm's registry keys: grok's.
    pub npm: &'a [NpmKey],
}

/// Verify the documents behind `version`.
pub(crate) fn verify(
    spec: &'static VendorSpec,
    triple: &str,
    fetcher: &dyn Fetcher,
    version: Version,
    head_body: &[u8],
    keys: Keys<'_>,
    now: i64,
) -> Resolution {
    let verified = match spec.anchor {
        Anchor::AnthropicOpenPgp => verify_claude(spec, triple, fetcher, version, keys.claude, now),
        Anchor::OpenAiTwoHost => verify_codex(spec, triple, fetcher, version, head_body),
        Anchor::GoogleTwoHost => verify_agy(spec, triple, fetcher, version, head_body),
        Anchor::MetaDeveloperId => verify_muse(spec, triple, fetcher, version),
        Anchor::MoonshotTwoHost => verify_kimi(spec, triple, fetcher, version),
        Anchor::XaiNpmRegistry => verify_grok(spec, triple, fetcher, version, keys.npm, now),
        Anchor::XaiDeveloperId => refused(String::from(
            "xAI's Developer ID alone no longer vouches for a build aterm installs",
        )),
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
        Anchor::MoonshotTwoHost => kimi_platform(triple).is_some(),
        Anchor::XaiNpmRegistry => grok_platform(triple).is_some(),
        Anchor::XaiDeveloperId => false,
    }
}

/// Admits a synthetic row before any byte moves, and again before it is staged: its one
/// digest is the one `vouch` names.
///
/// # Errors
/// [`FlowError::VendorRefused`] naming the first field that does not fit.
pub(crate) fn admit(row: &SyntheticArtifact, vouch: &Vouch) -> Result<(), FlowError> {
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
    let d = vouch.digest();
    let digest_ok = d.alg() == alg
        && a.digest()
            .is_some_and(|(got, hex)| got == alg && alg.is_lower_hex(hex) && hex == d.as_str());
    if !digest_ok || a.size == 0 {
        return refuse("a vendor-direct row needs its anchor's one digest and an exact size");
    }
    if a.asset != asset_name(row.program, row.version, &a.target, &a.payload) {
        return refuse("asset is not the per-version local name");
    }
    Ok(())
}

/// The staging name `spec`'s payload of `version` downloads to on `triple` ([`asset_name`],
/// [`payload_of`]).
#[cfg(test)]
pub(crate) fn staging_asset(spec: &VendorSpec, version: Version, triple: &str) -> String {
    asset_name(
        spec.program,
        version,
        triple,
        payload_of(spec.anchor, triple),
    )
}

/// Every local staging name `spec`'s payload of `version` may download to on `triple`
/// ([`asset_name`], [`payload_of`], [`verify_codex`]).
pub(crate) fn staging_assets(spec: &VendorSpec, version: Version, triple: &str) -> Vec<String> {
    if spec.anchor == Anchor::OpenAiTwoHost && triple.contains("-windows-") {
        return vec![
            asset_name(spec.program, version, triple, "zip"),
            asset_name(spec.program, version, triple, "tar-gz"),
        ];
    }
    vec![asset_name(
        spec.program,
        version,
        triple,
        payload_of(spec.anchor, triple),
    )]
}

fn payload_of(anchor: Anchor, triple: &str) -> &'static str {
    let windows = triple.contains("-windows-");
    match anchor {
        Anchor::AnthropicOpenPgp
        | Anchor::MetaDeveloperId
        | Anchor::MoonshotTwoHost
        | Anchor::XaiDeveloperId => "raw-binary",
        Anchor::XaiNpmRegistry => crate::vendor::NPM_BROTLI_BINARY,
        Anchor::GoogleTwoHost if windows => "raw-binary",
        Anchor::GoogleTwoHost => "tar-gz-binary",
        Anchor::OpenAiTwoHost if windows => "zip",
        Anchor::OpenAiTwoHost => "tar-gz",
    }
}

fn asset_name(program: &str, version: Version, triple: &str, payload: &str) -> String {
    let ext = match payload {
        "tar-gz" | "tar-gz-binary" => ".tar.gz",
        crate::vendor::NPM_BROTLI_BINARY => ".tgz",
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

/// A plain-text head (claude's, kimi's, grok's): one canonical version, then at most one `\n`.
fn plain_head_version(body: &[u8]) -> Option<Version> {
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
    let row = vouched(
        spec,
        version,
        triple,
        &url,
        Vouch::Documents(digest),
        size,
        payload,
    );
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
    let vouch = Vouch::Documents(digest);
    Ok(vouched(spec, version, triple, &url, vouch, size, payload))
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
    let vouch = Vouch::Documents(digest);
    Ok(vouched(
        spec, version, triple, &head.url, vouch, size, payload,
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
    let vouch = Vouch::Documents(digest);
    Ok(vouched(spec, version, triple, &url, vouch, size, payload))
}

/// kimi's manifest platform key and payload file for `triple`: darwin, Linux on glibc and
/// Windows, as Moonshot's release manifests spell them (no musl build is published).
pub(super) fn kimi_platform(triple: &str) -> Option<(&'static str, &'static str)> {
    Some(match triple {
        "aarch64-apple-darwin" => ("darwin-arm64", "kimi-code-darwin-arm64"),
        "x86_64-apple-darwin" => ("darwin-x64", "kimi-code-darwin-x64"),
        "x86_64-unknown-linux-gnu" => ("linux-x64", "kimi-code-linux-x64"),
        "aarch64-unknown-linux-gnu" => ("linux-arm64", "kimi-code-linux-arm64"),
        "x86_64-pc-windows-msvc" => ("win32-x64", "kimi-code-win32-x64.exe"),
        "aarch64-pc-windows-msvc" => ("win32-arm64", "kimi-code-win32-arm64.exe"),
        _ => return None,
    })
}

/// The `/platforms/<platform>/checksum` one kimi release manifest (`which`) names, once the
/// manifest is strict JSON for `version` under `tag` and names `file` for `platform`.
fn kimi_checksum(
    doc: &[u8],
    which: &str,
    version: Version,
    tag: &str,
    (platform, file): (&str, &str),
) -> Result<String, String> {
    let Ok(doc) = strict_json(doc) else {
        return Err(format!(
            "kimi's {which} release manifest is not strict JSON (a key repeats or it does not parse)"
        ));
    };
    let field = |v: &aterm_json::Value, key: &str| {
        v.get(key)
            .and_then(aterm_json::Value::as_str)
            .map(str::to_string)
    };
    if field(&doc, "version") != Some(version.to_string()) {
        return Err(format!(
            "kimi's {which} release manifest is not for {version}"
        ));
    }
    if field(&doc, "tag").as_deref() != Some(tag) {
        return Err(format!(
            "kimi's {which} release manifest is not tagged {tag}"
        ));
    }
    let Some(row) = doc.pointer(&format!("/platforms/{platform}")) else {
        return Err(format!(
            "kimi's {which} release manifest names no {platform} build"
        ));
    };
    if field(row, "filename").as_deref() != Some(file) {
        return Err(format!(
            "kimi's {which} release manifest names a {platform} file other than {file}"
        ));
    }
    field(row, "checksum")
        .ok_or_else(|| format!("kimi's {which} release manifest carries no sha256 for {platform}"))
}

fn verify_kimi(
    spec: &'static VendorSpec,
    triple: &str,
    fetcher: &dyn Fetcher,
    version: Version,
) -> Result<Candidate, Fault> {
    let Some(platform) = kimi_platform(triple) else {
        return refused(format!(
            "Moonshot AI publishes no kimi build aterm can verify for {triple}"
        ));
    };
    let tag = format!("@moonshot-ai/kimi-code@{version}");
    let alg = spec.anchor.digest_alg();
    let named = |which: &str, url: &str| -> Result<AuthenticatedDigest, Fault> {
        let doc = digest_doc(spec, fetcher, url, MANIFEST_CAP)?;
        let checksum =
            kimi_checksum(&doc, which, version, &tag, platform).map_err(Fault::Refused)?;
        AuthenticatedDigest::authenticated(alg, &checksum).ok_or_else(|| {
            Fault::Refused(format!(
                "kimi's {which} release manifest's {alg} for {} is not 64 hex digits",
                platform.0
            ))
        })
    };
    let cdn = named(
        "CDN",
        &format!("{KIMI_CDN}binaries/{version}/manifest.json"),
    )?;
    let github = named("GitHub", &format!("{KIMI_GITHUB}{tag}/manifest.json"))?;
    if cdn != github {
        return refused(format!(
            "kimi's CDN and GitHub release manifests disagree on {}'s {alg}",
            platform.1
        ));
    }
    let url = format!("{KIMI_CDN}binaries/{version}/{}", platform.1);
    let size = fetcher.vendor_content_length(spec.program, &url)?;
    if size == 0 {
        return refused(format!("{url} answered no content length"));
    }
    let payload = payload_of(spec.anchor, triple);
    let vouch = Vouch::Documents(cdn);
    Ok(vouched(spec, version, triple, &url, vouch, size, payload))
}

/// grok's platform for `triple`: npm's package for it, the words npm's documents name it by,
/// and the name xAI's bucket gives its binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GrokPlatform {
    /// npm's package platform: `@xai-official/grok-<npm>`.
    pub npm: &'static str,
    /// npm's `os` word.
    pub os: &'static str,
    /// npm's `cpu` word.
    pub cpu: &'static str,
    /// The bucket's: `grok-<version>-<bucket>`.
    pub bucket: &'static str,
}

impl GrokPlatform {
    /// npm's package name.
    #[must_use]
    pub(crate) fn package(self) -> String {
        format!("@xai-official/grok-{}", self.npm)
    }

    /// npm's version document for `version`.
    #[must_use]
    pub(crate) fn version_doc(self, version: Version) -> String {
        format!("{NPM_REGISTRY}{}/{version}", self.package())
    }

    /// npm's tarball of `version`, at the URL npm's own client builds for it.
    #[must_use]
    pub(crate) fn tarball(self, version: Version) -> String {
        format!(
            "{NPM_REGISTRY}{}/-/grok-{}-{version}.tgz",
            self.package(),
            self.npm
        )
    }

    /// xAI's bucket object of `version`'s binary.
    #[must_use]
    pub(crate) fn binary(self, version: Version) -> String {
        format!("{GROK_BUCKET}grok-{version}-{}", self.bucket)
    }

    /// What npm's documents of `version` must say ([`super::npm::Expected`]).
    #[must_use]
    pub(crate) fn expected<'a>(
        self,
        package: &'a str,
        version: Version,
        tarball: &'a str,
    ) -> super::npm::Expected<'a> {
        super::npm::Expected {
            name: package,
            version,
            tarball,
            os: self.os,
            cpu: self.cpu,
        }
    }
}

/// grok's platform for `triple`: darwin and Linux (xAI's Linux builds are static, so a musl
/// client runs the same one), as npm's `@xai-official/grok-<plat>` packages and xAI's
/// bucket spell them.
pub(crate) fn grok_platform(triple: &str) -> Option<GrokPlatform> {
    let (npm, os, cpu, bucket) = match triple {
        "aarch64-apple-darwin" => ("darwin-arm64", "darwin", "arm64", "macos-aarch64"),
        "x86_64-apple-darwin" => ("darwin-x64", "darwin", "x64", "macos-x86_64"),
        "x86_64-unknown-linux-gnu" | "x86_64-unknown-linux-musl" => {
            ("linux-x64", "linux", "x64", "linux-x86_64")
        }
        "aarch64-unknown-linux-gnu" | "aarch64-unknown-linux-musl" => {
            ("linux-arm64", "linux", "arm64", "linux-aarch64")
        }
        _ => return None,
    };
    Some(GrokPlatform {
        npm,
        os,
        cpu,
        bucket,
    })
}

/// A size `url` answers, held to `cap`.
fn sized(spec: &VendorSpec, fetcher: &dyn Fetcher, url: &str, cap: u64) -> Result<u64, Fault> {
    let size = fetcher.vendor_content_length(spec.program, url)?;
    if size == 0 {
        return refused(format!("{url} answered no content length"));
    }
    if size > cap {
        return refused(format!("{url} is {size} bytes, over its {cap}-byte bound"));
    }
    Ok(size)
}

/// grok's candidate: npm's registry-signed tarball of the head's version, the sha512 its
/// version document's integrity names (signed by a compiled registry key), sized by npm; the
/// binary's exact size and its bucket `.zst` beside it, sized by xAI's bucket. No payload byte
/// moves.
fn verify_grok(
    spec: &'static VendorSpec,
    triple: &str,
    fetcher: &dyn Fetcher,
    version: Version,
    keys: &[NpmKey],
    now: i64,
) -> Result<Candidate, Fault> {
    let Some(platform) = grok_platform(triple) else {
        return refused(format!(
            "xAI publishes no grok build aterm can verify for {triple}"
        ));
    };
    let (package, tarball) = (platform.package(), platform.tarball(version));
    let doc = digest_doc(spec, fetcher, &platform.version_doc(version), MANIFEST_CAP)?;
    let sha512 = super::npm::verify_dist(
        &doc,
        &platform.expected(&package, version, &tarball),
        keys,
        now,
    )
    .map_err(Fault::Refused)?;
    let size = sized(spec, fetcher, &tarball, GROK_MAX_DOWNLOAD)?;
    let binary = platform.binary(version);
    let binary_size = sized(spec, fetcher, &binary, GROK_MAX_BINARY)?;
    let zst = format!("{binary}.zst");
    let zst_size = sized(spec, fetcher, &zst, GROK_MAX_DOWNLOAD)?;
    let payload = payload_of(spec.anchor, triple);
    let vouch = Vouch::Documents(sha512);
    let mut c = vouched(spec, version, triple, &tarball, vouch, size, payload);
    c.artifact.artifact.cost.disk_installed = binary_size;
    c.companion = Some(Companion {
        url: zst,
        size: zst_size,
        binary_size,
        platform,
    });
    Ok(c)
}

fn vouched(
    spec: &'static VendorSpec,
    version: Version,
    triple: &str,
    url: &str,
    vouch: Vouch,
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
    let (sha256, sha512) = vouch.digest().keyed();
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
        vouch,
        companion: None,
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

/// `bytes` as JSON in which no object repeats a key, at any depth.
pub(super) fn strict_json(bytes: &[u8]) -> Result<aterm_json::Value, ()> {
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
#[path = "resolve_tests.rs"]
mod tests;
