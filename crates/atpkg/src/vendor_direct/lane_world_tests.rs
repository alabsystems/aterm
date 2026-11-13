// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::super::npm::{NpmKey, keyid_of};
use super::super::resolve::{self, GrokPlatform};
use super::super::version::Version;
use super::Trust;
use crate::flow::{Fetcher, VendorFetchError, VendorGet};
use crate::store::Layout;

/// 2026-09-22T00:00:00Z.
pub(crate) const NOW: i64 = 1_790_035_200;
const SIGNED_AT: u32 = 1_790_000_000;
/// The claude documents' base.
pub(crate) const CLAUDE: &str = "https://downloads.claude.ai/claude-code-releases/";
/// The codex head.
pub(crate) const CODEX_HEAD: &str = "https://releases.openai.com/codex/channels/latest";
/// Where codex's `SHA256SUMS` and packages live.
pub(crate) const CODEX_DL: &str = "https://github.com/openai/codex/releases/download/";

/// The triple the tests resolve for: the real host's OS and architecture, so
/// the CLI's native selection and the fixture's signed artifacts agree; a Windows
/// host resolves as Linux on its architecture, since the world publishes no
/// Windows artifact.
pub(crate) fn triple() -> &'static str {
    match (cfg!(target_os = "macos"), cfg!(target_arch = "aarch64")) {
        (true, true) => "aarch64-apple-darwin",
        (true, false) => "x86_64-apple-darwin",
        (false, true) => "aarch64-unknown-linux-gnu",
        (false, false) => "x86_64-unknown-linux-gnu",
    }
}

fn platform() -> &'static str {
    match (cfg!(target_os = "macos"), cfg!(target_arch = "aarch64")) {
        (true, true) => "darwin-arm64",
        (true, false) => "darwin-x64",
        (false, true) => "linux-arm64",
        (false, false) => "linux-x64",
    }
}

/// `bin/<name>` as this host's shim runs it: `bin/<name>.exe` on Windows, where a
/// payload lands under the executable spelling (`ToolName::exe_file`).
pub(crate) fn bin(name: &str) -> String {
    format!("bin/{name}{}", crate::platform::EXE_SUFFIX)
}

/// A "binary" in this platform's native executable format, distinct per `tag`.
pub(crate) fn native_exe(tag: &str) -> Vec<u8> {
    let mut bytes = if cfg!(target_os = "macos") {
        let mut bytes = 0xCFFA_EDFEu32.to_be_bytes().to_vec();
        bytes.extend_from_slice(&[0u8; 60]);
        bytes
    } else {
        crate::testdir::elf_fixture()
    };
    bytes.extend_from_slice(tag.as_bytes());
    bytes
}

fn pass_check(
    _: &Path,
    _: &str,
    _: std::time::Instant,
) -> Result<(), aterm_update_core::codesign::CodesignError> {
    Ok(())
}

fn refuse_check(
    _: &Path,
    _: &str,
    _: std::time::Instant,
) -> Result<(), aterm_update_core::codesign::CodesignError> {
    Err(aterm_update_core::codesign::CodesignError::Refused {
        code: Some(3),
        stderr: "test-requirement: code failed to satisfy specified code requirement(s)\n".into(),
    })
}

fn timeout_check(
    _: &Path,
    _: &str,
    _: std::time::Instant,
) -> Result<(), aterm_update_core::codesign::CodesignError> {
    Err(aterm_update_core::codesign::CodesignError::TimedOut)
}

/// [`trust`] with a codesign stand-in that never finishes by its deadline.
pub(crate) fn trust_timing_out() -> Trust {
    Trust {
        team_check: timeout_check,
        ..trust()
    }
}

/// The test keys and a codesign stand-in that passes.
pub(crate) fn trust() -> Trust {
    Trust {
        claude_key: crate::openpgp::testkit::test_key(),
        team_check: pass_check,
        npm_keys: npm_test_keys(),
    }
}

/// The signing half of the test npm registry key: `npm-test-key.p8`, a P-256 PKCS#8 key
/// made for these tests (`openssl genpkey -algorithm EC -pkeyopt
/// ec_paramgen_curve:P-256 -pkeyopt ec_param_enc:named_curve`, then `openssl pkcs8
/// -topk8 -nocrypt -outform DER`).
fn npm_signer() -> ring::signature::EcdsaKeyPair {
    ring::signature::EcdsaKeyPair::from_pkcs8(
        &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
        include_bytes!("fixtures/npm-test-key.p8"),
        &ring::rand::SystemRandom::new(),
    )
    .unwrap()
}

/// The test npm registry key, as [`Trust::npm_keys`] holds it.
pub(crate) fn npm_test_keys() -> &'static [NpmKey] {
    static KEYS: std::sync::OnceLock<[NpmKey; 1]> = std::sync::OnceLock::new();
    KEYS.get_or_init(|| {
        use ring::signature::KeyPair as _;
        let point: [u8; 65] = npm_signer().public_key().as_ref().try_into().unwrap();
        [NpmKey {
            keyid: Box::leak(keyid_of(&point).into_boxed_str()),
            point,
            not_after: None,
        }]
    })
}

/// The test registry key's signature over `message`, base64 as npm serves it.
pub(crate) fn npm_sign(message: &str) -> String {
    let sig = npm_signer()
        .sign(&ring::rand::SystemRandom::new(), message.as_bytes())
        .unwrap();
    aterm_codec::base64::encode(sig.as_ref()).unwrap()
}

/// grok's npm and bucket names for [`triple`]; `None` where xAI serves no build for the
/// real host (Windows, which [`triple`] stands in for as Linux, while the stager names a
/// Windows binary `grok.exe`), so a test's landing half could not run.
pub(crate) fn grok_platform() -> Option<GrokPlatform> {
    resolve::grok_platform(crate::cli::current_triple())?;
    resolve::grok_platform(triple())
}

/// What [`Fake::publish_grok`] served.
pub(crate) struct GrokServed {
    /// npm's tarball URL.
    pub tarball: String,
    /// xAI's bucket `.zst` of the binary.
    pub zst: String,
    /// xAI's bucket binary, which is only ever sized.
    pub binary: String,
    /// The tarball's sha512, lowercase hex.
    pub sha512: String,
}

/// `exe` as a Brotli stream ([`aterm_brotli::write_uncompressed`]).
pub(crate) fn brotli(exe: &[u8]) -> Vec<u8> {
    let mut br = Vec::new();
    aterm_brotli::write_uncompressed(&mut br, exe).unwrap();
    br
}

/// `exe` as one checksummed zstd frame, as xAI's bucket serves its `.zst`.
pub(crate) fn zstd(exe: &[u8]) -> Vec<u8> {
    let mut zst = Vec::new();
    aterm_zstd::frame::write_raw(&mut zst, exe, aterm_zstd::frame::Checksum::Xxh64).unwrap();
    zst
}

/// The `package/package.json` npm's package of grok `version` for this host carries.
pub(crate) fn grok_package_json(platform: GrokPlatform, version: &str) -> String {
    format!(
        "{{\n  \"name\": \"{}\",\n  \"version\": \"{version}\",\n  \"license\": \
         \"Apache-2.0\",\n  \"os\": [\"{}\"],\n  \"cpu\": [\"{}\"]\n}}\n",
        platform.package(),
        platform.os,
        platform.cpu
    )
}

/// A gzip'd tar of `members` (`(path, type flag, link, bytes, mode)`) and nothing else:
/// no directory entries, as npm packs a package.
pub(crate) fn npm_tgz(members: &[(&str, u8, &str, &[u8], u32)]) -> Vec<u8> {
    crate::extract::fixtures::gzip_bytes(&crate::extract::fixtures::tar_bytes(members))
}

/// npm's package of grok `version` for this host as xAI packs it: `package/bin/grok.br`
/// (`exe`), `package/package.json`, `package/README.md` and the third-party notices.
pub(crate) fn grok_tgz(platform: GrokPlatform, version: &str, exe: &[u8]) -> Vec<u8> {
    let json = grok_package_json(platform, version);
    npm_tgz(&[
        ("package/bin/grok.br", b'0', "", &brotli(exe), 0o644),
        ("package/package.json", b'0', "", json.as_bytes(), 0o644),
        ("package/README.md", b'0', "", b"# Grok Build\n", 0o644),
        (
            "package/THIRD_PARTY_NOTICES.md",
            b'0',
            "",
            &[b'n'; 7995],
            0o644,
        ),
    ])
}

pub(crate) fn sha512_hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(ring::digest::digest(&ring::digest::SHA512, bytes).as_ref())
}

/// [`trust`] with a codesign stand-in that refuses every Mach-O.
pub(crate) fn trust_refusing_signers() -> Trust {
    Trust {
        team_check: refuse_check,
        ..trust()
    }
}

pub(crate) fn sha(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// One served document.
#[derive(Clone)]
struct Doc {
    body: Vec<u8>,
    effective: Option<String>,
}

type LegacyPkgKey = (String, u64);

type SignedDoc = (Vec<u8>, Vec<u8>);

/// One of [`Fake`]'s fields: a lock, since a pass works its fetcher from several threads
/// at once ([`Fetcher::shared`]), with `RefCell`'s and `Cell`'s calls so the tests read
/// it like either.
#[derive(Default)]
pub(crate) struct Shared<T>(std::sync::RwLock<T>);

impl<T> Shared<T> {
    pub(crate) fn borrow(&self) -> std::sync::RwLockReadGuard<'_, T> {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn borrow_mut(&self) -> std::sync::RwLockWriteGuard<'_, T> {
        self.0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl<T: Copy> Shared<T> {
    pub(crate) fn get(&self) -> T {
        *self.borrow()
    }

    pub(crate) fn set(&self, value: T) {
        *self.borrow_mut() = value;
    }
}

/// The fake fetcher.
#[derive(Default)]
pub(crate) struct Fake {
    docs: Shared<BTreeMap<String, Doc>>,
    payloads: Shared<BTreeMap<String, Vec<u8>>>,
    /// Payloads only ever sized, at a size no test holds in memory.
    sized: Shared<BTreeMap<String, u64>>,
    /// Every `vendor_get`, `(url, if-none-match)`.
    pub gets: Shared<Vec<(String, Option<String>)>>,
    /// Every payload download's URL.
    pub downloads: Shared<Vec<String>>,
    /// Every vendor host unreachable.
    pub offline: Shared<bool>,
    /// Payload downloads unreachable (the documents still answer).
    pub downloads_fail: Shared<bool>,
    /// A signed index (bytes, signature) the index lane may resolve.
    pub index: Shared<Option<(Vec<u8>, Vec<u8>)>>,
    /// With no index published, its host answers with a refusal (a 403) rather than not
    /// at all: a rate-limited or revoked-token machine, never an offline one.
    pub index_refused: Shared<bool>,
    /// How many times the index was asked for.
    pub index_reads: Shared<u32>,
    /// Every head read as a hint (`vendor_head`), by URL; each is also in `gets`.
    pub head_reads: Shared<Vec<String>>,
    /// Signed legacy pkg manifests.
    pkgs: Shared<BTreeMap<LegacyPkgKey, SignedDoc>>,
    /// The repo of every pkg manifest asked for.
    pub pkg_reads: Shared<Vec<String>>,
}

impl Fake {
    /// Serve `url` with `body`.
    pub(crate) fn doc(&self, url: &str, body: &[u8]) {
        self.docs.borrow_mut().insert(
            url.to_string(),
            Doc {
                body: body.to_vec(),
                effective: None,
            },
        );
    }

    /// Answer `url` as though redirected to `effective`.
    pub(crate) fn redirect(&self, url: &str, effective: &str) {
        if let Some(d) = self.docs.borrow_mut().get_mut(url) {
            d.effective = Some(effective.to_string());
        }
    }

    /// Serve `url`'s payload.
    pub(crate) fn payload(&self, url: &str, bytes: &[u8]) {
        self.payloads
            .borrow_mut()
            .insert(url.to_string(), bytes.to_vec());
    }

    /// Stop serving `url`: it answers 404.
    pub(crate) fn forget(&self, url: &str) {
        self.docs.borrow_mut().remove(url);
    }

    /// Stop serving the payload at `url`: it answers 404.
    pub(crate) fn forget_payload(&self, url: &str) {
        self.payloads.borrow_mut().remove(url);
        self.sized.borrow_mut().remove(url);
    }

    /// Answer `url`'s size as `size`, for a payload a test only sizes (a download of it
    /// fails).
    pub(crate) fn payload_sized(&self, url: &str, size: u64) {
        self.sized.borrow_mut().insert(url.to_string(), size);
    }

    /// The document served at `url`.
    pub(crate) fn body(&self, url: &str) -> Vec<u8> {
        self.docs.borrow()[url].body.clone()
    }

    /// Publish the signed pkg manifest of `program`'s legacy index build `build`,
    /// naming vendor `version`.
    pub(crate) fn publish_legacy_pkg(&self, program: &str, build: u64, version: &str) {
        let body = format!(
            "schema = 2\nprogram = \"{program}\"\nversion = \"{version}\"\n\
             build_number = {build}\nexposes = [\"{program}\"]\n"
        );
        let sig = crate::sig::testkit::sign(&crate::sig::testkit::MACHINE_SEED, body.as_bytes());
        self.pkgs
            .borrow_mut()
            .insert((program.to_string(), build), (body.into_bytes(), sig));
    }

    /// [`Fake::publish_legacy_pkg`] with an artifact for this triple, as a legacy index
    /// build shipped one — what an index lane could fetch if it ever planned a vendor
    /// program (its `download` panics, so a test sees the attempt).
    #[cfg(unix)]
    pub(crate) fn publish_legacy_pkg_with_artifact(
        &self,
        program: &str,
        build: u64,
        version: &str,
    ) {
        let body = format!(
            "schema = 2\nprogram = \"{program}\"\nversion = \"{version}\"\n\
             build_number = {build}\nexposes = [\"{program}\"]\n\
             [[artifact]]\ntarget = \"{}\"\nkind = \"binary\"\n\
             asset = \"{program}-{build}.tar.zst\"\nsha256 = \"{}\"\n\
             tree_root = \"{}\"\nsize = 100\n[artifact.cost]\ndisk_installed = 1\n",
            triple(),
            "0".repeat(64),
            "1".repeat(64)
        );
        let sig = crate::sig::testkit::sign(&crate::sig::testkit::MACHINE_SEED, body.as_bytes());
        self.pkgs
            .borrow_mut()
            .insert((program.to_string(), build), (body.into_bytes(), sig));
    }

    /// [`Fake::publish_legacy_pkg`], its one artifact for `target` carrying the signed
    /// `tree_root` `root` — what the lane keeps for a rollback to that build.
    pub(crate) fn publish_legacy_pkg_rooted(
        &self,
        program: &str,
        build: u64,
        version: &str,
        target: &str,
        root: &str,
    ) {
        self.publish_pkg_with(program, build, version, target, root, "binary");
    }

    /// Publish the signed pkg manifest of index program `program` at `build`, its one
    /// artifact for `target` carrying the signed `tree_root` `root` — what an index
    /// pass's root recovery reads.
    #[cfg(unix)]
    pub(crate) fn publish_pkg_root(&self, program: &str, build: u64, target: &str, root: &str) {
        self.publish_pkg_kind(program, build, target, root, "binary");
    }

    /// [`Fake::publish_pkg_root`] with the artifact's `kind` as given.
    #[cfg(unix)]
    pub(crate) fn publish_pkg_kind(
        &self,
        program: &str,
        build: u64,
        target: &str,
        root: &str,
        kind: &str,
    ) {
        self.publish_pkg_with(program, build, "0.1", target, root, kind);
    }

    fn publish_pkg_with(
        &self,
        program: &str,
        build: u64,
        version: &str,
        target: &str,
        root: &str,
        kind: &str,
    ) {
        let body = format!(
            "schema = 2\nprogram = \"{program}\"\nversion = \"{version}\"\n\
             build_number = {build}\nexposes = [\"{program}\"]\n\
             [[artifact]]\ntarget = \"{target}\"\nkind = \"{kind}\"\n\
             asset = \"{program}-{build}.tar.zst\"\nsha256 = \"{}\"\n\
             tree_root = \"{root}\"\nsize = 100\n[artifact.cost]\ndisk_installed = 1\n",
            "0".repeat(64)
        );
        let sig = crate::sig::testkit::sign(&crate::sig::testkit::MACHINE_SEED, body.as_bytes());
        self.pkgs
            .borrow_mut()
            .insert((program.to_string(), build), (body.into_bytes(), sig));
    }

    /// Stop serving every legacy pkg manifest.
    pub(crate) fn forget_legacy_pkgs(&self) {
        self.pkgs.borrow_mut().clear();
    }

    /// The claude payload URL for `version`.
    pub(crate) fn claude_payload_url(version: &str) -> String {
        format!("{CLAUDE}{version}/{}/claude", platform())
    }

    /// Publish claude `version` as Anthropic's latest: the head, a manifest signed by
    /// the test key naming `payload`'s digest and size, and the payload.
    pub(crate) fn publish_claude(&self, version: &str, payload: &[u8]) {
        self.publish_claude_dated(version, payload, "2026-09-21T20:55:27Z");
    }

    /// [`Fake::publish_claude`] with an explicit signed `buildDate`.
    pub(crate) fn publish_claude_dated(&self, version: &str, payload: &[u8], date: &str) {
        let manifest = format!(
            "{{\"version\":\"{version}\",\"buildDate\":\"{date}\",\"platforms\":{{\"{}\":\
             {{\"binary\":\"claude\",\"checksum\":\"{}\",\"size\":{}}}}}}}",
            platform(),
            sha(payload),
            payload.len()
        );
        self.publish_claude_manifest(version, manifest.as_bytes());
        self.payload(&Self::claude_payload_url(version), payload);
    }

    /// Serve `manifest`, signed by the test key, as claude `version` (and make it the head).
    pub(crate) fn publish_claude_manifest(&self, version: &str, manifest: &[u8]) {
        let url = format!("{CLAUDE}{version}/manifest.json");
        self.doc(&url, manifest);
        let sig = crate::openpgp::testkit::sign_detached(manifest, SIGNED_AT);
        self.doc(&format!("{url}.sig"), &sig);
        self.doc(
            &format!("{CLAUDE}latest"),
            format!("{version}\n").as_bytes(),
        );
    }

    /// Publish grok `version` as xAI's stable pointer names it, npm's registry signs it
    /// (under the test key) and xAI's bucket serves it: npm's version document and its
    /// tarball of `exe` ([`grok_tgz`]), and the bucket's raw `exe` and `.zst` of it. `None`
    /// where xAI serves no build for this host.
    pub(crate) fn publish_grok(&self, version: &str, exe: &[u8]) -> Option<GrokServed> {
        let platform = grok_platform()?;
        let served = self.publish_grok_tarball(version, &grok_tgz(platform, version, exe));
        self.publish_grok_bucket(version, exe, exe);
        Some(served)
    }

    /// Serve `tgz` as npm's tarball of grok `version` for this host, and its version
    /// document signed by the test key over `tgz`'s sha512; the head names `version`.
    pub(crate) fn publish_grok_tarball(&self, version: &str, tgz: &[u8]) -> GrokServed {
        let platform = grok_platform().unwrap();
        let v = Version::parse(version).unwrap();
        let digest = ring::digest::digest(&ring::digest::SHA512, tgz);
        let sha512 = aterm_codec::hex::encode_lower(digest.as_ref());
        let integrity = format!(
            "sha512-{}",
            aterm_codec::base64::encode(digest.as_ref()).unwrap()
        );
        let name = platform.package();
        let tarball = platform.tarball(v);
        let sig = npm_sign(&format!("{name}@{version}:{integrity}"));
        let doc = format!(
            "{{\"name\":\"{name}\",\"version\":\"{version}\",\"os\":[\"{}\"],\
             \"cpu\":[\"{}\"],\"dist\":{{\"integrity\":\"{integrity}\",\"tarball\":\
             \"{tarball}\",\"signatures\":[{{\"keyid\":\"{}\",\"sig\":\"{sig}\"}}]}}}}",
            platform.os,
            platform.cpu,
            npm_test_keys()[0].keyid
        );
        self.doc(
            super::super::spec("grok").unwrap().head_url,
            version.as_bytes(),
        );
        self.doc(&platform.version_doc(v), doc.as_bytes());
        self.payload(&tarball, tgz);
        let binary = platform.binary(v);
        GrokServed {
            tarball,
            zst: format!("{binary}.zst"),
            binary,
            sha512,
        }
    }

    /// Serve xAI's bucket copy of grok `version` for this host: a raw object as long as
    /// `raw`, and the `.zst` of `zst`.
    pub(crate) fn publish_grok_bucket(&self, version: &str, raw: &[u8], zst: &[u8]) {
        let binary = grok_platform()
            .unwrap()
            .binary(Version::parse(version).unwrap());
        self.payload(&binary, raw);
        self.payload(&format!("{binary}.zst"), &zstd(zst));
    }

    /// The codex package name for [`triple`].
    pub(crate) fn codex_asset() -> String {
        Self::codex_asset_for(triple())
    }

    /// The codex package name for the target `target`.
    fn codex_asset_for(target: &str) -> String {
        format!("codex-package-{target}.tar.gz")
    }

    /// The codex payload URL for `version`.
    pub(crate) fn codex_payload_url(version: &str) -> String {
        Self::codex_payload_url_for(version, triple())
    }

    /// The codex payload URL for `version` on the target `target`.
    fn codex_payload_url_for(version: &str, target: &str) -> String {
        format!(
            "{CODEX_DL}rust-v{version}/{}",
            Self::codex_asset_for(target)
        )
    }

    /// Publish codex `version`: a tarball of `files` (`(path, bytes, mode)`), its
    /// `SHA256SUMS` on GitHub and the release JSON (the head) agreeing on its digest.
    pub(crate) fn publish_codex(&self, version: &str, files: &[(&str, &[u8], u32)]) {
        self.publish_codex_for(triple(), version, files);
    }

    /// [`Fake::publish_codex`], its package for the target `target` rather than this host's.
    pub(crate) fn publish_codex_for(
        &self,
        target: &str,
        version: &str,
        files: &[(&str, &[u8], u32)],
    ) {
        let tarball = tarball(files);
        let digest = sha(&tarball);
        let asset = Self::codex_asset_for(target);
        let release = format!(
            "{{\"tag_name\":\"rust-v{version}\",\"assets\":[{{\"name\":\"{asset}\",\
             \"digest\":\"sha256:{digest}\",\"browser_download_url\":\
             \"https://releases.openai.com/codex/releases/{version}/{asset}\"}}]}}"
        );
        self.doc(CODEX_HEAD, release.as_bytes());
        let sums = format!(
            "{}  codex-app-server-package-{target}.tar.gz\n{digest}  {asset}\n",
            "0".repeat(64),
        );
        self.doc(
            &format!("{CODEX_DL}rust-v{version}/codex-package_SHA256SUMS"),
            sums.as_bytes(),
        );
        self.payload(&Self::codex_payload_url_for(version, target), &tarball);
    }

    /// The codex tree a genuine package lays: the entry, a helper, and data — one
    /// executable-mode data file among it, as 0.156.0 ships.
    pub(crate) fn publish_codex_default(&self, version: &str) {
        self.publish_codex_default_for(triple(), version);
    }

    /// [`Fake::publish_codex_default`], its package for the target `target` rather than this
    /// host's.
    pub(crate) fn publish_codex_default_for(&self, target: &str, version: &str) {
        let codex = native_exe(&format!("codex {version}"));
        let helper = native_exe("rg");
        self.publish_codex_for(
            target,
            version,
            &[
                (&bin("codex"), &codex, 0o755),
                ("codex-path/rg", &helper, 0o755),
                ("codex-resources/voice/runtime.json", b"{\"a\": 1}\n", 0o755),
                ("codex-resources/voice/NOTICE.md", b"# Notices\n", 0o644),
            ],
        );
    }
}

/// A gzip'd tar of `files`, with their directories.
pub(crate) fn tarball(files: &[(&str, &[u8], u32)]) -> Vec<u8> {
    let mut dirs: Vec<String> = Vec::new();
    for (path, _, _) in files {
        let mut acc = String::new();
        for part in path.split('/').collect::<Vec<_>>().split_last().unwrap().1 {
            acc.push_str(part);
            acc.push('/');
            if !dirs.contains(&acc) {
                dirs.push(acc.clone());
            }
        }
    }
    let mut entries: Vec<(&str, u8, &str, &[u8], u32)> = dirs
        .iter()
        .map(|d| (d.as_str(), b'5', "", &[][..], 0o755))
        .collect();
    entries.extend(
        files
            .iter()
            .map(|&(path, bytes, mode)| (path, b'0', "", bytes, mode)),
    );
    let tar = crate::extract::fixtures::tar_bytes(&entries);
    crate::extract::fixtures::gzip_bytes(&tar)
}

fn unreachable<T>(what: &str) -> Result<T, VendorFetchError> {
    Err(VendorFetchError::Unreachable(format!("offline: {what}")))
}

impl Fetcher for Fake {
    fn index_candidates(&self) -> Result<Vec<crate::select::Candidate>, String> {
        *self.index_reads.borrow_mut() += 1;
        let Some((index_bytes, sig)) = self.index.borrow().clone() else {
            return Err(String::from(if self.index_refused.get() {
                "HTTP 403 for the index listing: API rate limit exceeded"
            } else {
                "no index is published"
            }));
        };
        let (roster_bytes, roster_sig) = crate::sig::testkit::published_roster();
        Ok(vec![crate::select::Candidate {
            label: "v0".into(),
            index_bytes,
            sig,
            roster_bytes,
            roster_sig,
        }])
    }
    fn index_link_down(&self) -> bool {
        !self.index_refused.get()
    }
    fn shared(&self) -> Option<&(dyn Fetcher + Sync)> {
        Some(self)
    }
    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        self.pkg_reads.borrow_mut().push(repo.to_string());
        self.pkgs
            .borrow()
            .get(&(program.to_string(), build))
            .cloned()
            .ok_or_else(|| String::from("no manifest"))
    }
    fn download(&self, _: &str, asset: &str, _: &Path) -> Result<(), String> {
        panic!("the vendor lane never reaches the release lane (asked for {asset})")
    }
    fn download_url(&self, url: &str, _: &Path, _: u64) -> Result<(), String> {
        panic!("the vendor lane never reaches the index's https lane (asked for {url})")
    }
    fn vendor_get(
        &self,
        program: &str,
        url: &str,
        cap: u64,
        if_none_match: Option<&str>,
    ) -> Result<VendorGet, VendorFetchError> {
        assert!(
            crate::vendor::vendor_direct_url_allowed(program, url),
            "{program} asked for an unpinned {url}"
        );
        self.gets
            .borrow_mut()
            .push((url.to_string(), if_none_match.map(str::to_string)));
        if self.offline.get() {
            return unreachable(url);
        }
        let Some(doc) = self.docs.borrow().get(url).cloned() else {
            return Err(VendorFetchError::Refused(format!("404 for {url}")));
        };
        let etag = format!("\"{}\"", &sha(&doc.body)[..12]);
        if if_none_match == Some(etag.as_str()) {
            return Ok(VendorGet::NotModified { etag });
        }
        if doc.body.len() as u64 > cap {
            return Err(VendorFetchError::Refused(format!("{url} is over its cap")));
        }
        Ok(VendorGet::Body {
            bytes: doc.body,
            etag: Some(etag),
            effective_url: doc.effective.unwrap_or_else(|| url.to_string()),
        })
    }
    fn vendor_head(
        &self,
        program: &str,
        url: &str,
        cap: u64,
        if_none_match: Option<&str>,
    ) -> Result<VendorGet, VendorFetchError> {
        self.head_reads.borrow_mut().push(url.to_string());
        self.vendor_get(program, url, cap, if_none_match)
    }
    fn vendor_content_length(&self, program: &str, url: &str) -> Result<u64, VendorFetchError> {
        assert!(
            crate::vendor::vendor_direct_url_allowed(program, url),
            "{url}"
        );
        if self.offline.get() {
            return unreachable(url);
        }
        if let Some(size) = self.sized.borrow().get(url) {
            return Ok(*size);
        }
        self.payloads
            .borrow()
            .get(url)
            .map(|b| b.len() as u64)
            .ok_or_else(|| VendorFetchError::Refused(format!("404 for {url}")))
    }
    fn vendor_download(
        &self,
        program: &str,
        url: &str,
        dest: &Path,
        cap: u64,
    ) -> Result<(), VendorFetchError> {
        assert!(
            crate::vendor::vendor_direct_url_allowed(program, url),
            "{url}"
        );
        self.downloads.borrow_mut().push(url.to_string());
        if self.offline.get() || self.downloads_fail.get() {
            return unreachable(url);
        }
        let bytes = self
            .payloads
            .borrow()
            .get(url)
            .cloned()
            .ok_or_else(|| VendorFetchError::Refused(format!("404 for {url}")))?;
        if bytes.len() as u64 > cap {
            return Err(VendorFetchError::Refused(format!("{url} is over its cap")));
        }
        std::fs::write(dest, bytes).map_err(|e| VendorFetchError::Refused(e.to_string()))
    }
}

/// A signed index (bytes, machine signature) valid until `valid_until`, naming `ay` (an
/// ALab program with no published build), `claude` and `codex`, its stable channel
/// pinning all three with `yanked` as given.
pub(crate) fn signed_index(
    index_build: u64,
    valid_until: &str,
    yanked: &[&str],
) -> (Vec<u8>, Vec<u8>) {
    signed_index_naming(index_build, valid_until, yanked, &["claude", "codex"])
}

/// [`signed_index`], with `vendors` the only vendor programs it names and pins.
pub(crate) fn signed_index_naming(
    index_build: u64,
    valid_until: &str,
    yanked: &[&str],
    vendors: &[&str],
) -> (Vec<u8>, Vec<u8>) {
    let yanked: Vec<String> = yanked.iter().map(|y| format!("\"{y}\"")).collect();
    let blocks: String = vendors
        .iter()
        .map(|p| format!("[programs.{p}]\nrepo = \"aterm\"\n"))
        .collect();
    let pins: String = vendors
        .iter()
        .map(|p| format!(", {p} = 2026091901"))
        .collect();
    let body = format!(
        "schema = 2\nindex_build = {index_build}\nvalid_until = \"{valid_until}\"\n\
         machine_id = \"{}\"\nroster_seq = {}\n\
         [programs.ay]\nrepo = \"ay\"\n{blocks}\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
         yanked = [{}]\n\
         pin = {{ ay = 18{pins} }}\n",
        crate::sig::testkit::MACHINE_ID,
        crate::sig::testkit::SEQ,
        yanked.join(", ")
    );
    let sig = crate::sig::testkit::sign(&crate::sig::testkit::MACHINE_SEED, body.as_bytes());
    (body.into_bytes(), sig)
}

/// A fresh 0700 prefix under the temp dir, removed when the test ends.
pub(crate) fn layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-vlane-{label}-"))
}

/// The store directory of `program`'s vendor `version`.
pub(crate) fn build_dir(layout: &Layout, program: &str, version: &str) -> PathBuf {
    let v = crate::vendor_direct::Version::parse(version).unwrap();
    layout.build_dir(program, v.build_id())
}
