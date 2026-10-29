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
    const KIMI_LATEST: &[u8] = include_bytes!("fixtures/kimi-latest");
    const KIMI_CDN_MANIFEST: &[u8] = include_bytes!("fixtures/kimi-2.3.1-cdn-manifest.json");
    const KIMI_GITHUB_MANIFEST: &[u8] = include_bytes!("fixtures/kimi-2.3.1-github-manifest.json");
    const GROK_STABLE: &[u8] = include_bytes!("fixtures/grok-stable");
    const NOW: i64 = 1_790_035_200;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn spec(program: &str) -> &'static VendorSpec {
        super::super::spec(program).unwrap()
    }

    /// Anthropic's `key` beside npm's compiled registry keys.
    fn keys(key: &PinnedRsaKey) -> Keys<'_> {
        Keys {
            claude: key,
            npm: super::super::npm::NPM_REGISTRY_KEYS,
        }
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
                keys(&key),
                NOW,
            ));
            let a = c.artifact.artifact();
            assert_eq!(c.version, v("2.1.280"));
            assert_eq!(c.vouch.digest().as_str(), sha, "{triple}");
            assert_eq!(a.sha256, sha);
            assert_eq!(a.size, size);
            assert_eq!(a.url, format!("{CLAUDE}2.1.280/{platform}/{suffix}"));
            assert_eq!(a.payload, "raw-binary");
            assert_eq!(a.entry, "claude");
            let ext = if suffix.ends_with(".exe") { ".exe" } else { "" };
            assert_eq!(a.asset, format!("claude-2.1.280-{triple}{ext}"));
            assert_eq!(c.build_date, BuildDate::parse("2026-09-21T20:55:27Z"));
            admit(&c.artifact, &c.vouch).unwrap();
        }
        let test_key = crate::openpgp::testkit::test_key();
        match verify(
            spec("claude"),
            "aarch64-apple-darwin",
            &f,
            v("2.1.280"),
            b"",
            keys(&test_key),
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
            keys(&key),
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
            keys(&key),
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
                keys(&key),
                NOW,
            ));
            let a = c.artifact.artifact();
            assert_eq!(c.vouch.digest().as_str(), sha, "{triple}");
            assert_eq!(a.url, url);
            assert_eq!(a.size, 1631, "the exact cap is the HEAD content length");
            assert_eq!(a.payload, "tar-gz");
            assert_eq!(a.asset, format!("codex-0.156.0-{triple}.tar.gz"));
            assert_eq!(c.build_date, None);
            admit(&c.artifact, &c.vouch).unwrap();
        }
    }

    #[test]
    fn the_heads_name_canonical_versions_only() {
        assert_eq!(plain_head_version(b"2.1.280\n"), Some(v("2.1.280")));
        assert_eq!(plain_head_version(b"2.1.280"), Some(v("2.1.280")));
        for bad in [
            &b"2.1.280\n\n"[..],
            b" 2.1.280",
            b"2.1.0280",
            b"2.1",
            b"latest",
            b"",
        ] {
            assert_eq!(plain_head_version(bad), None, "{bad:?}");
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
            staging_assets(spec("codex"), v("0.156.0"), "aarch64-pc-windows-msvc"),
            [
                "codex-0.156.0-aarch64-pc-windows-msvc.zip",
                "codex-0.156.0-aarch64-pc-windows-msvc.tar.gz"
            ]
        );
        assert_eq!(
            staging_assets(spec("codex"), v("0.156.0"), "x86_64-apple-darwin"),
            ["codex-0.156.0-x86_64-apple-darwin.tar.gz"]
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
        let c = candidate(verify(
            spec("claude"),
            "aarch64-apple-darwin",
            &f,
            v("2.1.280"),
            b"",
            keys(&key),
            NOW,
        ));
        let (good, vouch) = (c.artifact.clone(), c.vouch.clone());
        admit(&good, &vouch).unwrap();
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
            Box::new(|a| a.sha256 = "b".repeat(64)),
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
            assert!(admit(&row, &vouch).is_err(), "tweak {i} was admitted");
        }
        let mut digestless = good.clone();
        digestless.artifact.sha256 = String::new();
        assert!(
            admit(&digestless, &vouch).is_err(),
            "a row carries the digest its anchor vouched for"
        );
        let a = good.artifact();
        let sha512 = AuthenticatedDigest::authenticated(DigestAlg::Sha512, &"5".repeat(128));
        let by_512 = vouched(
            spec("claude"),
            good.version,
            &a.target,
            &a.url,
            Vouch::Documents(sha512.unwrap()),
            a.size,
            &a.payload,
        );
        assert_eq!(
            by_512.artifact.artifact().digest(),
            Some((DigestAlg::Sha512, "5".repeat(128).as_str()))
        );
        assert!(by_512.artifact.artifact().sha256.is_empty());
        assert!(
            admit(&by_512.artifact, &by_512.vouch).is_err(),
            "claude's anchor vouches for a sha256"
        );
        let mut other = good;
        other.program = "codex";
        assert!(
            admit(&other, &vouch).is_err(),
            "a claude row is not codex's"
        );
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
        verify(spec("agy"), triple, f, v(version), head, keys(&key), NOW)
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
            assert_eq!(c.vouch.digest().alg(), DigestAlg::Sha512, "{triple}");
            assert_eq!(c.vouch.digest().as_str(), sha512);
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
            admit(&c.artifact, &c.vouch).unwrap();
            let mut by_256 = c.artifact.clone();
            by_256.artifact.sha512 = String::new();
            by_256.artifact.sha256 = "a".repeat(64);
            assert!(admit(&by_256, &c.vouch).is_err(), "{triple}");
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
                keys(&key),
                NOW,
            ));
            let a = c.artifact.artifact();
            assert_eq!(
                (c.vouch.digest().alg(), c.vouch.digest().as_str()),
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
            admit(&c.artifact, &c.vouch).unwrap();
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
                keys(&key),
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
                keys(&key),
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

    const KIMI_CDN_DOC: &str = "https://cdn.kimi.com/kimi-code/binaries/2.3.1/manifest.json";
    const KIMI_GITHUB_DOC: &str = "https://github.com/MoonshotAI/kimi-code/releases/download/\
                                   @moonshot-ai/kimi-code@2.3.1/manifest.json";
    const GITHUB_STORAGE: &str = "https://release-assets.githubusercontent.com/github-production-release-asset/1246459259/1d";

    fn verify_kimi_on(f: &Fake, triple: &str, version: &str) -> Resolution {
        let key = crate::openpgp::ANTHROPIC_CLAUDE_CODE_RELEASE_KEY;
        verify(spec("kimi"), triple, f, v(version), b"", keys(&key), NOW)
    }

    /// Both kimi 2.3.1 manifests as Moonshot serves them (the GitHub one through its storage
    /// redirect) and a sized payload for every served platform.
    fn kimi_world() -> Fake {
        let f = Fake::default();
        f.doc(KIMI_CDN_DOC, KIMI_CDN_MANIFEST);
        f.doc(KIMI_GITHUB_DOC, KIMI_GITHUB_MANIFEST);
        f.redirect(KIMI_GITHUB_DOC, GITHUB_STORAGE);
        for file in [
            "kimi-code-darwin-arm64",
            "kimi-code-darwin-x64",
            "kimi-code-linux-x64",
            "kimi-code-linux-arm64",
            "kimi-code-win32-x64.exe",
            "kimi-code-win32-arm64.exe",
        ] {
            f.payload(
                &format!("https://cdn.kimi.com/kimi-code/binaries/2.3.1/{file}"),
                &[0u8; 4321],
            );
        }
        f
    }

    #[test]
    fn the_real_kimi_release_resolves_when_the_cdn_and_github_agree() {
        let f = kimi_world();
        f.doc(spec("kimi").head_url, KIMI_LATEST);
        assert!(matches!(
            read_head(spec("kimi"), &f, None),
            Head::Named { version, .. } if version == v("2.3.1")
        ));
        for (triple, file, sha) in [
            (
                "aarch64-apple-darwin",
                "kimi-code-darwin-arm64",
                "f06ba6a3bb7ed2fc1f82590f7c22570736d9ee7344059fa92404ca75dc2d5b45",
            ),
            (
                "x86_64-apple-darwin",
                "kimi-code-darwin-x64",
                "c88f0d8ffa7d7a330199e63e0b9a4b8857e900c3b8f58fd5e90411d3e3ad1a87",
            ),
            (
                "x86_64-unknown-linux-gnu",
                "kimi-code-linux-x64",
                "781ec1c24e5e79a01aaaa31ad859e8818ae11ffe792e93645cdc0c068a264ba0",
            ),
            (
                "aarch64-unknown-linux-gnu",
                "kimi-code-linux-arm64",
                "9572c66402643ea16e97db239d739096a31454e74daa5ec12af04519e9916d68",
            ),
            (
                "x86_64-pc-windows-msvc",
                "kimi-code-win32-x64.exe",
                "93878ac2bd12a8cf21b4e148e7f7705a7a6252c1398fd34eb50f8998ba0bf795",
            ),
            (
                "aarch64-pc-windows-msvc",
                "kimi-code-win32-arm64.exe",
                "f15999373de18e1c84d5a5e96ddc0eb6ffdddaa3b056968c378117046b88fe26",
            ),
        ] {
            let c = candidate(verify_kimi_on(&f, triple, "2.3.1"));
            let a = c.artifact.artifact();
            assert_eq!(
                c.vouch,
                Vouch::Documents(
                    AuthenticatedDigest::authenticated(DigestAlg::Sha256, sha).unwrap()
                ),
                "{triple}"
            );
            assert_eq!((a.sha256.as_str(), a.sha512.as_str()), (sha, ""));
            assert_eq!(
                a.url,
                format!("https://cdn.kimi.com/kimi-code/binaries/2.3.1/{file}")
            );
            assert_eq!(a.size, 4321, "the exact cap is the HEAD content length");
            assert_eq!(
                (a.payload.as_str(), a.entry.as_str()),
                ("raw-binary", "kimi")
            );
            let ext = if triple.contains("-windows-") {
                ".exe"
            } else {
                ""
            };
            assert_eq!(a.asset, format!("kimi-2.3.1-{triple}{ext}"));
            assert_eq!((c.build_date, c.anchor), (None, Anchor::MoonshotTwoHost));
            admit(&c.artifact, &c.vouch).unwrap();
        }
    }

    #[test]
    fn kimi_refuses_two_manifests_that_do_not_vouch_for_one_payload() {
        const TRIPLE: &str = "aarch64-apple-darwin";
        let sha = "f06ba6a3bb7ed2fc1f82590f7c22570736d9ee7344059fa92404ca75dc2d5b45";
        let f = kimi_world();
        candidate(verify_kimi_on(&f, TRIPLE, "2.3.1"));
        for (url, doc, from, to, said) in [
            (
                KIMI_GITHUB_DOC,
                KIMI_GITHUB_MANIFEST,
                &sha[..8],
                "00000000",
                "disagree",
            ),
            (
                KIMI_CDN_DOC,
                KIMI_CDN_MANIFEST,
                &sha[..8],
                "00000000",
                "disagree",
            ),
            (
                KIMI_GITHUB_DOC,
                KIMI_GITHUB_MANIFEST,
                "\"tag\": \"@moonshot-ai/kimi-code@2.3.1\"",
                "\"tag\": \"@moonshot-ai/kimi-code@2.3.0\"",
                "not tagged @moonshot-ai/kimi-code@2.3.1",
            ),
            (
                KIMI_CDN_DOC,
                KIMI_CDN_MANIFEST,
                "\"filename\": \"kimi-code-darwin-arm64\"",
                "\"filename\": \"kimi-code-darwin-x64\"",
                "other than kimi-code-darwin-arm64",
            ),
            (
                KIMI_GITHUB_DOC,
                KIMI_GITHUB_MANIFEST,
                "\"version\": \"2.3.1\"",
                "\"version\": \"2.3.2\"",
                "GitHub release manifest is not for 2.3.1",
            ),
            (
                KIMI_CDN_DOC,
                KIMI_CDN_MANIFEST,
                sha,
                &sha[..63],
                "not 64 hex",
            ),
            (
                KIMI_GITHUB_DOC,
                KIMI_GITHUB_MANIFEST,
                "\"darwin-arm64\"",
                "\"darwin-arm\"",
                "names no darwin-arm64 build",
            ),
            (
                KIMI_CDN_DOC,
                KIMI_CDN_MANIFEST,
                "{",
                "{\"version\": \"2.3.1\",",
                "strict JSON",
            ),
        ] {
            f.doc(url, &edit(doc, from, to));
            if url == KIMI_GITHUB_DOC {
                f.redirect(url, GITHUB_STORAGE);
            }
            let why = refusal(verify_kimi_on(&f, TRIPLE, "2.3.1"));
            assert!(why.contains(said), "{to}: {why}");
            f.doc(url, doc);
            if url == KIMI_GITHUB_DOC {
                f.redirect(url, GITHUB_STORAGE);
            }
        }
        candidate(verify_kimi_on(&f, TRIPLE, "2.3.1"));
        f.payload(
            "https://cdn.kimi.com/kimi-code/binaries/2.3.1/kimi-code-darwin-arm64",
            b"",
        );
        let why = refusal(verify_kimi_on(&f, TRIPLE, "2.3.1"));
        assert!(why.contains("no content length"), "{why}");
        let next = kimi_world();
        next.doc(
            "https://cdn.kimi.com/kimi-code/binaries/2.3.2/manifest.json",
            KIMI_CDN_MANIFEST,
        );
        let why = refusal(verify_kimi_on(&next, TRIPLE, "2.3.2"));
        assert!(
            why.contains("CDN release manifest is not for 2.3.2"),
            "{why}"
        );
    }

    #[test]
    fn each_kimi_manifest_is_read_only_where_its_own_host_answers() {
        const TRIPLE: &str = "x86_64-unknown-linux-gnu";
        let f = kimi_world();
        for (url, effective) in [
            (KIMI_CDN_DOC, GITHUB_STORAGE),
            (
                KIMI_CDN_DOC,
                "https://cdn.kimi.com/kimi-code/binaries/2.3.2/manifest.json",
            ),
            (KIMI_GITHUB_DOC, "https://evil.example/manifest.json"),
            (KIMI_GITHUB_DOC, KIMI_CDN_DOC),
            (
                KIMI_GITHUB_DOC,
                "https://release-assets.githubusercontent.com.evil.com/x",
            ),
        ] {
            f.redirect(url, effective);
            let why = refusal(verify_kimi_on(&f, TRIPLE, "2.3.1"));
            assert!(
                why.contains("off its pinned host"),
                "{url} -> {effective}: {why}"
            );
            f.doc(url, &f.body(url));
            if url == KIMI_GITHUB_DOC {
                f.redirect(url, GITHUB_STORAGE);
            }
        }
        f.redirect(KIMI_GITHUB_DOC, "https://objects.githubusercontent.com/x");
        candidate(verify_kimi_on(&f, TRIPLE, "2.3.1"));
        f.doc(KIMI_GITHUB_DOC, KIMI_GITHUB_MANIFEST);
        candidate(verify_kimi_on(&f, TRIPLE, "2.3.1"));
        let head = spec("kimi").head_url;
        f.doc(head, KIMI_LATEST);
        f.redirect(head, "https://code.kimi.com/kimi-code/latest");
        assert!(matches!(
            read_head(spec("kimi"), &f, None),
            Head::Refused(_)
        ));
    }

    const GROK_BUCKET_CLI: &str = "https://storage.googleapis.com/grok-build-public-artifacts/cli/";
    const NPM_XAI: &str = "https://registry.npmjs.org/@xai-official/";
    const GROK_NPM_LINUX_X64: &[u8] = include_bytes!("fixtures/grok-1.0.70-npm-linux-x64.json");
    const GROK_NPM_LINUX_ARM64: &[u8] = include_bytes!("fixtures/grok-1.0.70-npm-linux-arm64.json");
    const GROK_NPM_DARWIN_ARM64: &[u8] =
        include_bytes!("fixtures/grok-1.0.70-npm-darwin-arm64.json");

    /// grok 1.0.70 as npm and xAI's bucket served it on 2026-10-24: the real version documents
    /// of three platforms, and every size the lane asks for, as measured that day.
    fn grok_world() -> Fake {
        let f = Fake::default();
        for (npm, doc) in [
            ("linux-x64", GROK_NPM_LINUX_X64),
            ("linux-arm64", GROK_NPM_LINUX_ARM64),
            ("darwin-arm64", GROK_NPM_DARWIN_ARM64),
        ] {
            f.doc(&format!("{NPM_XAI}grok-{npm}/1.0.70"), doc);
        }
        for (npm, bucket, tgz, raw, zst) in [
            (
                "linux-x64",
                "linux-x86_64",
                55_485_055,
                195_700_192,
                57_553_935,
            ),
            (
                "linux-arm64",
                "linux-aarch64",
                50_533_753,
                166_639_048,
                54_274_393,
            ),
            (
                "darwin-arm64",
                "macos-aarch64",
                49_911_569,
                179_063_008,
                53_423_354,
            ),
        ] {
            f.payload_sized(&format!("{NPM_XAI}grok-{npm}/-/grok-{npm}-1.0.70.tgz"), tgz);
            f.payload_sized(&format!("{GROK_BUCKET_CLI}grok-1.0.70-{bucket}"), raw);
            f.payload_sized(&format!("{GROK_BUCKET_CLI}grok-1.0.70-{bucket}.zst"), zst);
        }
        f
    }

    fn verify_grok_on(f: &Fake, triple: &str) -> Resolution {
        verify(
            spec("grok"),
            triple,
            f,
            v("1.0.70"),
            b"1.0.70",
            keys(&crate::openpgp::ANTHROPIC_CLAUDE_CODE_RELEASE_KEY),
            NOW,
        )
    }

    /// The real grok 1.0.70: on each platform npm and the bucket serve, the candidate is
    /// npm's tarball under the sha512 its registry-signed version document names, sized by
    /// npm, staged to the bucket's binary size and checked against the bucket's `.zst`; a
    /// musl host gets the same static build.
    #[test]
    fn the_real_grok_release_resolves_under_npms_registry_key() {
        let f = grok_world();
        assert!(matches!(
            read_head(spec("grok"), &f, None),
            Head::Unserved(_)
        ));
        f.doc(spec("grok").head_url, b"1.0.70");
        assert!(matches!(
            read_head(spec("grok"), &f, None),
            Head::Named { version, .. } if version == v("1.0.70")
        ));
        for (triple, npm, bucket, sha512, size, binary_size, zst_size) in [
            (
                "x86_64-unknown-linux-gnu",
                "linux-x64",
                "linux-x86_64",
                "ac496c872733ffa4a03d2922146b4a1c0e2f495388f2a43a7748c57aee08f768\
                 5708c7efc603223af352cee0619b63fc25dfb8acd180b72db35b45709a01d278",
                55_485_055,
                195_700_192,
                57_553_935,
            ),
            (
                "x86_64-unknown-linux-musl",
                "linux-x64",
                "linux-x86_64",
                "ac496c872733ffa4a03d2922146b4a1c0e2f495388f2a43a7748c57aee08f768\
                 5708c7efc603223af352cee0619b63fc25dfb8acd180b72db35b45709a01d278",
                55_485_055,
                195_700_192,
                57_553_935,
            ),
            (
                "aarch64-unknown-linux-gnu",
                "linux-arm64",
                "linux-aarch64",
                "ac7d137d6393fc26bfb0132d00927e189f54c9996bfd3913a881eaf052fba3e6\
                 53cc93015af0afe34242e13fd7bb0119dd6326c4e1c688f6a9ec7f41e126c34b",
                50_533_753,
                166_639_048,
                54_274_393,
            ),
            (
                "aarch64-apple-darwin",
                "darwin-arm64",
                "macos-aarch64",
                "4efcd9e1be98c2988aed4c9b246278017c400fb7f3f18f75d90d2db18e959280\
                 c5460a7121458175ba7a031f080899f54447fbaa7b0b71146eca268e4f025b9a",
                49_911_569,
                179_063_008,
                53_423_354,
            ),
        ] {
            let c = candidate(verify_grok_on(&f, triple));
            let a = c.artifact.artifact();
            assert_eq!(c.anchor, Anchor::XaiNpmRegistry);
            assert_eq!(c.vouch.digest().alg(), DigestAlg::Sha512, "{triple}");
            assert_eq!(c.vouch.digest().as_str(), sha512, "{triple}");
            assert_eq!((a.sha256.as_str(), a.sha512.as_str()), ("", sha512));
            assert_eq!(
                a.url,
                format!("{NPM_XAI}grok-{npm}/-/grok-{npm}-1.0.70.tgz")
            );
            assert_eq!((a.size, a.cost.disk_installed), (size, binary_size));
            assert_eq!(
                (a.payload.as_str(), a.entry.as_str()),
                ("npm-brotli-binary", "grok")
            );
            assert_eq!(a.asset, format!("grok-1.0.70-{triple}.tgz"));
            let companion = c.companion.as_ref().expect("the bucket's copy");
            assert_eq!(
                companion.url,
                format!("{GROK_BUCKET_CLI}grok-1.0.70-{bucket}.zst")
            );
            assert_eq!(
                (companion.size, companion.binary_size),
                (zst_size, binary_size)
            );
            assert_eq!(
                companion.platform.package(),
                format!("@xai-official/grok-{npm}")
            );
            admit(&c.artifact, &c.vouch).unwrap();
        }
        let asked: Vec<String> = f.gets.borrow().iter().map(|(u, _)| u.clone()).collect();
        assert!(
            asked.iter().all(|u| u == spec("grok").head_url
                || (u.starts_with(NPM_XAI) && u.ends_with("/1.0.70") && !u.contains("/grok/"))),
            "only the bucket's pointer and the platform packages' version documents: {asked:?}"
        );
    }

    /// Every way grok's documents and sizes fail: an unserved document or copy is no verdict
    /// (healing when served), and a document off its pin, under a key aterm does not ship, or
    /// a size out of bounds is refused.
    #[test]
    fn grok_is_unserved_or_refused_as_its_documents_and_sizes_say() {
        const T: &str = "x86_64-unknown-linux-gnu";
        let doc = format!("{NPM_XAI}grok-linux-x64/1.0.70");
        let tgz = format!("{NPM_XAI}grok-linux-x64/-/grok-linux-x64-1.0.70.tgz");
        let raw = format!("{GROK_BUCKET_CLI}grok-1.0.70-linux-x86_64");
        let zst = format!("{raw}.zst");
        for gone in [&doc, &tgz, &raw, &zst] {
            let f = grok_world();
            f.forget(gone);
            f.forget_payload(gone);
            match verify_grok_on(&f, T) {
                Resolution::Unserved(why) => assert!(why.contains("404"), "{gone}: {why}"),
                other => panic!("{gone}: {other:?}"),
            }
        }
        for (url, size, why) in [
            (&tgz, 0, "no content length"),
            (&tgz, (256 << 20) + 1, "over its"),
            (&raw, 0, "no content length"),
            (&raw, (1 << 30) + 1, "over its"),
            (&zst, (256 << 20) + 1, "over its"),
        ] {
            let f = grok_world();
            f.payload_sized(url, size);
            let why_got = refusal(verify_grok_on(&f, T));
            assert!(why_got.contains(why), "{url} {size}: {why_got}");
        }
        let f = grok_world();
        f.redirect(&doc, "https://registry.npmjs.org/@xai-official/grok/1.0.70");
        let why = refusal(verify_grok_on(&f, T));
        assert!(why.contains("off its pinned host"), "{why}");
        let f = grok_world();
        f.redirect(
            &doc,
            "https://registry.npmjs.org/@xai-official/grok-linux-x64/1.0.70",
        );
        candidate(verify_grok_on(&f, T));
        let f = grok_world();
        let why = refusal(verify(
            spec("grok"),
            T,
            &f,
            v("1.0.70"),
            b"1.0.70",
            Keys {
                claude: &crate::openpgp::ANTHROPIC_CLAUDE_CODE_RELEASE_KEY,
                npm: &[],
            },
            NOW,
        ));
        assert!(why.contains("aterm does not ship"), "{why}");
        assert!(
            matches!(
                verify(
                    spec("grok"),
                    T,
                    &f,
                    v("1.0.69"),
                    b"1.0.69",
                    keys(&crate::openpgp::ANTHROPIC_CLAUDE_CODE_RELEASE_KEY),
                    NOW,
                ),
                Resolution::Unserved(_)
            ),
            "a version npm has no document for"
        );
        f.doc(
            &format!("{NPM_XAI}grok-linux-x64/1.0.69"),
            GROK_NPM_LINUX_X64,
        );
        let why = refusal(verify(
            spec("grok"),
            T,
            &f,
            v("1.0.69"),
            b"1.0.69",
            keys(&crate::openpgp::ANTHROPIC_CLAUDE_CODE_RELEASE_KEY),
            NOW,
        ));
        assert!(
            why.contains("is version \"1.0.70\", not 1.0.69"),
            "the head's version, never the document's: {why}"
        );
        let f = grok_world();
        f.doc(spec("grok").head_url, b"1.0.70");
        f.redirect(spec("grok").head_url, "https://x.ai/cli/stable");
        assert!(matches!(
            read_head(spec("grok"), &f, None),
            Head::Refused(_)
        ));
    }

    /// grok's platforms: npm's package, its `os` and `cpu` words, and the bucket's name, for
    /// each triple xAI builds; none for Windows.
    #[test]
    fn grok_platforms_name_npms_package_and_the_buckets_binary() {
        for (triple, npm, os, cpu, bucket) in [
            (
                "aarch64-apple-darwin",
                "darwin-arm64",
                "darwin",
                "arm64",
                "macos-aarch64",
            ),
            (
                "x86_64-apple-darwin",
                "darwin-x64",
                "darwin",
                "x64",
                "macos-x86_64",
            ),
            (
                "x86_64-unknown-linux-gnu",
                "linux-x64",
                "linux",
                "x64",
                "linux-x86_64",
            ),
            (
                "x86_64-unknown-linux-musl",
                "linux-x64",
                "linux",
                "x64",
                "linux-x86_64",
            ),
            (
                "aarch64-unknown-linux-gnu",
                "linux-arm64",
                "linux",
                "arm64",
                "linux-aarch64",
            ),
            (
                "aarch64-unknown-linux-musl",
                "linux-arm64",
                "linux",
                "arm64",
                "linux-aarch64",
            ),
        ] {
            let p = grok_platform(triple).unwrap();
            assert_eq!(
                (p.npm, p.os, p.cpu, p.bucket),
                (npm, os, cpu, bucket),
                "{triple}"
            );
            let version = v("1.0.70");
            assert_eq!(
                p.tarball(version),
                format!("{NPM_XAI}grok-{npm}/-/grok-{npm}-1.0.70.tgz")
            );
            assert_eq!(
                p.version_doc(version),
                format!("{NPM_XAI}grok-{npm}/1.0.70")
            );
            assert_eq!(
                p.binary(version),
                format!("{GROK_BUCKET_CLI}grok-1.0.70-{bucket}")
            );
            for url in [
                p.tarball(version),
                p.version_doc(version),
                p.binary(version),
            ] {
                assert!(
                    crate::vendor::vendor_direct_url_allowed("grok", &url),
                    "{url}"
                );
            }
        }
        for triple in crate::manifest::TARGETS {
            assert_eq!(
                grok_platform(triple).is_some(),
                !triple.contains("-windows-"),
                "{triple}"
            );
        }
    }

    #[test]
    fn an_unserved_triple_is_refused_before_any_request() {
        let key = crate::openpgp::ANTHROPIC_CLAUDE_CODE_RELEASE_KEY;
        let f = kimi_world();
        for (program, triple, said) in [
            (
                "kimi",
                "x86_64-unknown-linux-musl",
                "no kimi build aterm can verify",
            ),
            (
                "kimi",
                "aarch64-unknown-linux-musl",
                "no kimi build aterm can verify",
            ),
            (
                "grok",
                "x86_64-pc-windows-msvc",
                "no grok build aterm can verify",
            ),
            (
                "grok",
                "aarch64-pc-windows-msvc",
                "no grok build aterm can verify",
            ),
        ] {
            let row = spec(program);
            assert!(!row.serves(triple), "{program} {triple}");
            let version = if program == "kimi" { "2.3.1" } else { "1.0.70" };
            let why = refusal(verify(row, triple, &f, v(version), b"", keys(&key), NOW));
            assert!(why.contains(said), "{program} {triple}: {why}");
        }
        assert!(f.gets.borrow().is_empty(), "nothing was asked");
        for triple in crate::manifest::TARGETS {
            assert_eq!(
                spec("kimi").serves(triple),
                !triple.ends_with("-musl"),
                "kimi on {triple}"
            );
            assert_eq!(
                spec("grok").serves(triple),
                !triple.contains("-windows-"),
                "grok on {triple}"
            );
        }
    }

    #[test]
    fn the_plain_heads_name_one_canonical_version() {
        assert_eq!(plain_head_version(KIMI_LATEST), Some(v("2.3.1")));
        assert_eq!(plain_head_version(GROK_STABLE), Some(v("1.0.67")));
        for bad in [
            &b"1.0.67\n\n"[..],
            b"1.0.67 ",
            b"v1.0.67",
            b"1.0",
            b"2.4.0-beta.1\n",
        ] {
            assert_eq!(plain_head_version(bad), None, "{bad:?}");
        }
        let f = Fake::default();
        f.doc(spec("grok").head_url, b"1.0.70-R1");
        match read_head(spec("grok"), &f, None) {
            Head::Refused(why) => assert!(why.contains("grok's grammar"), "{why}"),
            other => panic!("{other:?}"),
        }
    }
}
