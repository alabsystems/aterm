// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The signed manifest schemas (§4) and the discovery allow-list (§5), parsed only after signature verification.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::sig::{Reject, VerifiedBytes};

/// The highest manifest `schema` this build understands; a higher one is rejected (the client stays put) rather than misread, as `aterm_update_core::SUPPORTED_SCHEMA` is.
pub const SUPPORTED_SCHEMA: u32 = 2;

/// The repository the signed index lives on, under the configurable account: `github.com/<account>/aterm` (§5), as a small release asset on the aterm repo itself.
pub const INDEX_REPO: &str = "aterm";

/// The machine-signed `index.toml` (§4.1): allow-list, attribution, freshness and channels.
#[derive(Debug, Clone, Deserialize)]
pub struct Index {
    /// Manifest format version; `> SUPPORTED_SCHEMA` is refused.
    pub schema: u32,
    /// Monotonic index counter: the durable high-water rollback floor (§8).
    pub index_build: u64,
    /// RFC3339 generation time (informational).
    #[serde(default)]
    pub generated_at: String,
    /// RFC3339 freshness deadline; the client refuses this index at or after it (§8, checked by the caller via [`crate::sig::check_freshness`]).
    pub valid_until: String,
    /// Which roster machine cut this index: the attribution half of the id bind ([`aterm_update_core::roster::Attribution::bind`]).
    #[serde(default)]
    pub machine_id: Option<String>,
    /// The roster generation that authorized the signing machine.
    #[serde(default)]
    pub roster_seq: Option<u64>,
    /// `[programs.<name>]` — the open-ended allow-list.
    #[serde(default)]
    pub programs: BTreeMap<String, Program>,
    /// `[[channels]]`: named, pinned program sets (`stable`/`nightly`).
    #[serde(default)]
    pub channels: Vec<Channel>,
}

/// One `[programs.<name>]` entry: where the program's release manifests live and how it may be installed.
#[derive(Debug, Clone, Deserialize)]
pub struct Program {
    /// The GitHub repo (under the same account) carrying this program's `pkg-*.toml`.
    pub repo: String,
    /// `"prebuilt-only"` | `"prebuilt-or-build"` (§6).
    #[serde(default)]
    pub policy: String,
    /// Coherence group: members of one group apply atomically as one tuple (§7).
    #[serde(default)]
    pub coherence_group: Option<String>,
    /// `system = "gh"`: a binary of this name on the user's `PATH`, outside the managed `bin/` and the store, satisfies the program — no download, no shim, status `satisfied by system: <path>`, reconciled every pass ([`crate::vendor::system_satisfied`]).
    #[serde(default)]
    pub system: Option<String>,
    /// The one-line reason a target with no `[[artifact]]` row cannot have this program (`"Emacs is a macOS-only member"`), shown verbatim in the `unavailable on <target>: <hint>` state ([`crate::state::unavailable`]).
    #[serde(default)]
    pub unavailable_hint: Option<String>,
    /// `requires = […]`: retired.
    #[serde(default)]
    pub requires: Vec<String>,
}

impl Program {
    /// Whether the signed row names the retired `requires` relation; the install door and the update pass both refuse such a program ([`crate::FlowError::RequiresRetired`]).
    #[inline]
    #[must_use]
    pub const fn names_requires(&self) -> bool {
        !self.requires.is_empty()
    }
}

/// One `[[channels]]` entry: a named, pinned set of program builds plus the gating counters.
#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    /// Channel name (`stable`, `nightly`).
    pub name: String,
    /// Monotonic no-downgrade gate for the channel's coherence group.
    #[serde(default)]
    pub channel_build: u64,
    /// Channel-wide yank floor: a pinned build below it is force-upgraded or tombstoned at apply (§7).
    #[serde(default)]
    pub min_build: u64,
    /// Per-program yank floors (`program -> floor`), for a channel whose members have independent build counters.
    #[serde(default)]
    pub min_build_by_program: BTreeMap<String, u64>,
    /// Per-program revocations (`"trust@4790"`), enforced at apply (§7).
    #[serde(default)]
    pub yanked: Vec<String>,
    /// The pinned set: exact per-program builds that move together (`program -> build`).
    #[serde(default)]
    pub pin: BTreeMap<String, u64>,
    /// Per-target pin overlays (`triple -> (program -> build)`), laid over [`Self::pin`] by [`Self::for_target`], so one target can pin its own build without moving another's.
    #[serde(default)]
    pub pin_by_target: BTreeMap<String, BTreeMap<String, u64>>,
}

impl Channel {
    /// The effective yank floor for one program: the max of [`Self::min_build`] and that program's [`Self::min_build_by_program`] entry.
    #[inline]
    #[must_use]
    pub fn min_build_for(&self, program: &str) -> u64 {
        self.min_build
            .max(self.min_build_by_program.get(program).copied().unwrap_or(0))
    }

    /// This channel as seen from one target: [`Self::pin`] with `pin_by_target[triple]` laid over it (an overlay entry replaces that program's pin or adds a program).
    #[must_use]
    pub fn for_target(&self, triple: &str) -> Channel {
        let mut view = self.clone();
        if let Some(overlay) = self.pin_by_target.get(triple) {
            for (program, build) in overlay {
                view.pin.insert(program.clone(), *build);
            }
        }
        view.pin_by_target.clear();
        view
    }
}

impl Index {
    /// The named channel as seen from `triple` ([`Channel::for_target`]), or `None` when the index has no such channel.
    #[must_use]
    pub fn channel_for(&self, name: &str, triple: &str) -> Option<Channel> {
        self.channels
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.for_target(triple))
    }

    /// The named program, iff the verified index names it.
    #[inline]
    #[must_use]
    pub fn program(&self, name: &str) -> Option<&Program> {
        self.programs.get(name)
    }

    /// Whether `name` is a program the index names (§5): an unlisted name is not installable.
    #[inline]
    #[must_use]
    pub fn is_program(&self, name: &str) -> bool {
        self.programs.contains_key(name)
    }

    /// The installable set: every program the index names, less the narrowing-only `[packages].exclude` (R4/§5).
    /// It is also the default set the passes keep complete: every program, an ALab harness
    /// ([`crate::harness`]) included, is on unless a person turns it off.
    #[must_use]
    pub fn installable(&self, exclude: &[String]) -> BTreeSet<String> {
        let mut set: BTreeSet<String> = self.programs.keys().cloned().collect();
        for e in exclude {
            set.remove(e);
        }
        set
    }
}

/// A machine-signed `pkg-<program>-<build>.toml` (§4.2): the per-triple artifact matrix for one program build.
#[derive(Debug, Clone, Deserialize)]
pub struct PkgManifest {
    /// Manifest format version; `> SUPPORTED_SCHEMA` is refused.
    pub schema: u32,
    /// The program name, which must equal the index `[programs]` key that pointed here.
    pub program: String,
    /// Human version string (informational / display).
    #[serde(default)]
    pub version: String,
    /// Monotonic build number: the strictly-greater downgrade gate.
    pub build_number: u64,
    /// The binaries to shim into `bin/` (§10); a program may expose several (R2).
    #[serde(default)]
    pub exposes: Vec<String>,
    /// Runtime dependencies: other index-named programs this build needs, each missing one installed first ([`crate::flow::install`]); a yanked, below-floor, unreachable, unpinned or cyclic dep is skipped with a warning.
    #[serde(default)]
    pub requires: Vec<String>,
    /// `shim_env = ["NAME=VALUE", …]`: the environment every shim of this program (primary and `alab-` alias) exports before it execs the store binary ([`crate::shim_env`]), so a managed vendor tool runs with its own updater off (`DISABLE_AUTOUPDATER=1` for Claude Code) and the index re-pin is its update path.
    #[serde(default)]
    pub shim_env: Vec<String>,
    /// `[[artifact]]`: one row per target triple.
    #[serde(default, rename = "artifact")]
    pub artifacts: Vec<Artifact>,
}

/// The six target triples a program may carry rows for (§17.1) — the spelling a row's `target` uses and the one the client reports for itself (`cli::current_triple`: a spelling outside this list selects no row, and every install test under it fails).
pub const TARGETS: &[&str] = &[
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
];

/// The tools the published trust bundle shims (the pkg-trust-6808 set): the producer packs and refuses a seal without each.
pub const TRUST_EXPOSES: &[&str] = &[
    "trustc",
    "targo",
    "targo-trust",
    "targo-fmt",
    "targo-tippy",
    "tippy",
    "tippy-driver",
    "trustfmt",
    "trustdoc",
    "trust-analyzer",
    "targo-trust-wp",
    "trust-wp-driver",
    "trust-wp",
    "cargo-trust-wp",
    "trust-wp-rustc",
];

/// One `[[artifact]]` row: how one target triple obtains and applies this build.
#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    /// Target triple, one of [`TARGETS`].
    pub target: String,
    /// The payload/apply shape: `binary` | `elf-binary` | `cargo-src` | `sysroot-bundle` |
    /// `app-bundle` (§4.2/§17). `elf-binary` is a GNU-only release row whose actual ELF runtime
    /// requirements must be admitted; older clients refuse that selected kind before download.
    #[serde(default)]
    pub kind: String,
    /// Where the bytes come from: `github-release` (the default: a release asset under the account slug) or `https` (a vendor's own download, pinned by this signed row).
    #[serde(default = "default_protocol")]
    pub protocol: String,
    /// `github-release`: the release asset file name to download.
    #[serde(default)]
    pub asset: String,
    /// SHA-256 of the downloaded bytes: the download-integrity gate.
    #[serde(default)]
    pub sha256: String,
    /// SHA-512 of the downloaded bytes, for a vendor-direct row whose vendor publishes only that; empty is absent.
    #[serde(default)]
    pub sha512: String,
    /// SHA-256 over the sorted extracted-file list: the apply-time re-verify root (§8 TOCTOU).
    #[serde(default)]
    pub tree_root: String,
    /// Asset size in bytes: drives the per-artifact download cap and disk preflight.
    #[serde(default)]
    pub size: u64,
    /// Relocation policy for a `sysroot-bundle` (§10.1).
    #[serde(default = "default_reloc")]
    pub reloc: String,
    /// `[artifact.cost]`: accounting shown before any byte moves (R7).
    #[serde(default)]
    pub cost: Cost,
    /// `https`: the vendor's per-version URL the client downloads from.
    #[serde(default)]
    pub url: String,
    /// `https` only: how the download is staged: `raw-binary` | `tar-gz-binary` | `tar-gz` | `tar-zst` | `zip` (each with `kind = "binary"`) | `dmg` (with `kind = "app-bundle"`) ([`crate::vendor::PAYLOADS`]).
    #[serde(default)]
    pub payload: String,
    /// `raw-binary`/`tar-gz-binary` only: the tool name the download (or the tar's one file) becomes under `bin/` (mode `0755`), in the platform's executable spelling ([`crate::store::ToolName::exe_file`]).
    #[serde(default)]
    pub entry: String,
    /// Archive payloads only (`tar-gz`/`tar-zst`/`zip`): leading path components to drop on extraction (`gh_<ver>_macOS_arm64/bin/gh` → `bin/gh`).
    #[serde(default)]
    pub strip_components: u32,
    /// `dmg` (and, if declared, archive) payloads: relative symlinks created at `bin/<name>` → `../<target>` after staging, so the shims resolve `bin/<tool>` (`emacs = "Emacs.app/Contents/MacOS/Emacs"`).
    #[serde(default)]
    pub links: BTreeMap<String, String>,
    /// Display-only vendor name for the consent copy (`"Anthropic PBC"`).
    #[serde(default)]
    pub vendor: String,
    /// `[artifact.provenance]`: which compiler built this row's payload, as the packer's lineage scan recorded it ([`crate::lineage`]).
    #[serde(default)]
    pub provenance: Option<Provenance>,
}

impl Artifact {
    /// The signed digest that gates this row's download: whichever one of `sha256` and `sha512` it sets ([`one_digest`]).
    #[inline]
    #[must_use]
    pub fn digest(&self) -> Option<(DigestAlg, &str)> {
        one_digest(&self.sha256, &self.sha512)
    }
}

/// A payload digest's algorithm, and the key every file that carries one names it by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DigestAlg {
    /// SHA-256: every index row, and the vendors that publish it.
    Sha256,
    /// SHA-512: a vendor that publishes only that.
    Sha512,
}

impl DigestAlg {
    /// The key a digest of this algorithm is spelled under (`sha256 = …`, `sha512=…`).
    #[inline]
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        }
    }

    /// Whether `hex` is one digest of this algorithm, in lowercase hex.
    #[inline]
    #[must_use]
    pub fn is_lower_hex(self, hex: &str) -> bool {
        let len = match self {
            Self::Sha256 => 64,
            Self::Sha512 => 128,
        };
        aterm_codec::hex::is_lower(hex, len)
    }
}

impl std::fmt::Display for DigestAlg {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.key())
    }
}

/// The one digest a `sha256`/`sha512` pair names (a signed row's, a log entry's, a record's): `(algorithm, value)` when exactly one is non-empty, `None` otherwise.
#[inline]
#[must_use]
pub const fn one_digest<'a>(sha256: &'a str, sha512: &'a str) -> Option<(DigestAlg, &'a str)> {
    match (sha256.is_empty(), sha512.is_empty()) {
        (false, true) => Some((DigestAlg::Sha256, sha256)),
        (true, false) => Some((DigestAlg::Sha512, sha512)),
        _ => None,
    }
}

/// `[artifact.provenance]`: the scan record of the packer that signed the row.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct Provenance {
    /// `release:` of the compiler that built the payload (`1.99.0-dev`).
    #[serde(default)]
    pub compiler_release: String,
    /// Its `commit-hash:` — 40 hex.
    #[serde(default)]
    pub compiler_commit: String,
    /// Its `trust:` product version.
    #[serde(default)]
    pub compiler_trust: String,
    /// Its `host:` triple.
    #[serde(default)]
    pub compiler_host: String,
    /// sha256 of its `bin/trustc`.
    #[serde(default)]
    pub trustc_sha256: String,
    /// sha256 of its `bin/targo`.
    #[serde(default)]
    pub targo_sha256: String,
    /// sha256 of its compiler driver.
    #[serde(default)]
    pub driver_sha256: String,
    /// How its lineage was proven: `lineage`, `seed-mint`, or `unproven` (admitted while the receipt requirement is dormant).
    #[serde(default)]
    pub lineage: String,
    /// sha256 of the SEED-MINT receipt, or `none`.
    #[serde(default)]
    pub lineage_receipt_sha256: String,
    /// The receipt's double-compile relation, or `none`.
    #[serde(default)]
    pub ddc_seed_relation: String,
    /// Compiled objects the scan read.
    #[serde(default)]
    pub scan_objects: String,
    /// Evidence of another compiler it found (always `0` in a signed row): none found, not none possible (see [`crate::lineage`]).
    #[serde(default)]
    pub scan_findings: String,
}

/// A `sysroot-bundle` with no explicit policy is `self-contained`.
fn default_reloc() -> String {
    "self-contained".to_string()
}

/// A row with no `protocol` is a release asset under the account slug.
fn default_protocol() -> String {
    "github-release".to_string()
}

/// The retired `kind` spelling, and the split that replaced it — the message
/// [`Reject::RetiredKind`] carries.
const VENDOR_FETCH_RETIRED: &str = "kind = \"vendor-fetch\" is retired (it was never \
    published): a row now declares its payload SHAPE as kind = \"binary\" (or \
    \"app-bundle\" with payload = \"dmg\") and where the bytes come from as \
    protocol = \"https\"";

/// `[artifact.cost]`: the structural accounting block (§4.2/R7).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Cost {
    /// Bytes downloaded.
    #[serde(default)]
    pub download_bytes: u64,
    /// Bytes resident after install.
    #[serde(default)]
    pub disk_installed: u64,
    /// `0` ⇒ prebuilt; nonzero ⇒ from-source build estimate (seconds).
    #[serde(default)]
    pub build_seconds: u64,
}

impl PkgManifest {
    /// The admitted shim environment ([`crate::shim_env::ShimEnv`]).
    #[inline]
    #[must_use]
    pub fn shim_env(&self) -> crate::shim_env::ShimEnv {
        crate::shim_env::ShimEnv::admit(&self.shim_env).unwrap_or_default()
    }

    /// The artifact for `target`, if this build ships one.
    #[inline]
    #[must_use]
    pub fn artifact_for(&self, target: &str) -> Option<&Artifact> {
        self.artifacts.iter().find(|a| a.target == target)
    }

    /// Whether the signed `program` field is `name`: the anti-replay bind (§4.2).
    #[inline]
    #[must_use]
    pub fn is_for(&self, name: &str) -> bool {
        self.program == name
    }
}

/// Parse a verified `index.toml`: strict UTF-8 (the signature covers these exact bytes), a real `toml` parse, then the [`SUPPORTED_SCHEMA`] reject-newer gate.
pub(crate) fn parse_index(verified: &VerifiedBytes) -> Result<Index, Reject> {
    let idx: Index = parse_toml(verified)?;
    if idx.schema > SUPPORTED_SCHEMA {
        return Err(Reject::Schema);
    }
    Ok(idx)
}

/// Parse a verified `pkg-*.toml` (the same strict UTF-8 and reject-newer discipline as [`parse_index`]), then its row and `shim_env` rules.
pub fn parse_pkg(verified: &VerifiedBytes) -> Result<PkgManifest, Reject> {
    let m: PkgManifest = parse_toml(verified)?;
    if m.schema > SUPPORTED_SCHEMA {
        return Err(Reject::Schema);
    }
    validate_rows(&m)?;
    validate_shim_env(&m)?;
    Ok(m)
}

/// The `shim_env` list must be one a shim can honour ([`crate::shim_env::ShimEnv::admit`]),
/// checked once at parse so no writer meets an entry it cannot embed.
fn validate_shim_env(m: &PkgManifest) -> Result<(), Reject> {
    crate::shim_env::ShimEnv::admit(&m.shim_env)
        .map(|_| ())
        .map_err(Reject::ShimEnv)
}

/// The row rules `serde` cannot express (`asset`/`sha256` are `serde`-optional, so a row of an
/// unknown protocol can omit them and be refused by name at admission):
///
/// * the retired `kind = "vendor-fetch"` refuses the whole manifest ([`Reject::RetiredKind`]);
/// * a `github-release` or `https` row requires `asset` and `sha256` and carries no `sha512`
///   ([`Reject::Malformed`]).
fn validate_rows(m: &PkgManifest) -> Result<(), Reject> {
    for a in &m.artifacts {
        if a.kind == "vendor-fetch" {
            return Err(Reject::RetiredKind(VENDOR_FETCH_RETIRED));
        }
        let release_shaped = matches!(a.protocol.as_str(), "github-release" | "https");
        if release_shaped && (a.asset.is_empty() || a.sha256.is_empty() || !a.sha512.is_empty()) {
            return Err(Reject::Malformed);
        }
    }
    Ok(())
}

/// Strict UTF-8 and `toml` deserialize over verified bytes. Invalid UTF-8 or any TOML or shape
/// error (missing field, duplicate key, wrong type) is [`Reject::Malformed`].
fn parse_toml<T: serde::de::DeserializeOwned>(verified: &VerifiedBytes) -> Result<T, Reject> {
    let text = std::str::from_utf8(verified.as_slice()).map_err(|_| Reject::Malformed)?;
    aterm_toml::from_str(text).map_err(|_| Reject::Malformed)
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
