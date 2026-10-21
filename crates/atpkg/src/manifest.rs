// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The signed manifest schemas (§4) and the discovery allow-list (§5), parsed only after
//! signature verification.
//!
//! * [`Index`]: the machine-signed `index.toml` — the freshness anchor, the attribution pair
//!   (`machine_id`/`roster_seq`) binding it to the roster generation that authorized it, the
//!   allow-list of installable programs, and the channels. A repo not named in `[programs]` is
//!   unreachable (R4): private-config repos are never named.
//! * [`PkgManifest`]: a `pkg-<program>-<build>.toml` signed by a machine on the same roster —
//!   the per-triple artifact table, the `exposes` shim list and the `[cost]`.
//!
//! The parse entry points ([`parse_index`], [`parse_pkg`]) take `&`[`VerifiedBytes`], which has
//! no public constructor, so the crate's parse path cannot run on unverified input (the schema
//! structs derive `Deserialize` only so callers can read results). The real `toml` parser makes
//! a duplicate key an error and table scoping intrinsic. Both carry the [`SUPPORTED_SCHEMA`]
//! reject-newer gate.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::sig::{Reject, VerifiedBytes};

/// The highest manifest `schema` this build understands; a higher one is rejected (the client
/// stays put) rather than misread, as `aterm_update_core::SUPPORTED_SCHEMA` is. A schema-1
/// index (the retired `[keys]` delegation, no attribution) parses but is refused at the
/// attribution bind ([`Reject::Unattributed`]).
pub const SUPPORTED_SCHEMA: u32 = 2;

/// The repository the signed index lives on, under the configurable account:
/// `github.com/<account>/aterm` (§5), as a small release asset on the aterm repo itself. Not
/// overridable ([`crate::discovery::index_repo`]).
pub const INDEX_REPO: &str = "aterm";

/// The machine-signed `index.toml` (§4.1): allow-list, attribution, freshness and channels.
/// Unknown top-level keys and tables are ignored; a newer schema is rejected by
/// [`parse_index`].
#[derive(Debug, Clone, Deserialize)]
pub struct Index {
    /// Manifest format version; `> SUPPORTED_SCHEMA` is refused.
    pub schema: u32,
    /// Monotonic index counter: the durable high-water rollback floor (§8). Required.
    pub index_build: u64,
    /// RFC3339 generation time (informational).
    #[serde(default)]
    pub generated_at: String,
    /// RFC3339 freshness deadline; the client refuses this index at or after it (§8, checked
    /// by the caller via [`crate::sig::check_freshness`]).
    pub valid_until: String,
    /// Which roster machine cut this index: the attribution half of the id bind
    /// ([`aterm_update_core::roster::Attribution::bind`]). Inside the signed bytes, so an m3
    /// signature cannot be relabelled m11 and a thief holding m11's key cannot claim m3.
    /// `Option` only so a document lacking it parses; absent is a refusal under an armed anchor
    /// ([`crate::sig::Reject::Unattributed`]), never a pass.
    #[serde(default)]
    pub machine_id: Option<String>,
    /// The roster generation that authorized the signing machine. Bound to the roster actually
    /// used, so an old roster cannot be paired with a new index; a newer roster with an older
    /// index is admitted (the roster travels on the channel head). Absent ⇒
    /// [`crate::sig::Reject::SeqMismatch`].
    #[serde(default)]
    pub roster_seq: Option<u64>,
    /// `[programs.<name>]` — the open-ended allow-list. The map key is the program name
    /// (`exposes`/install identity); the value names its repo + policy + optional group.
    #[serde(default)]
    pub programs: BTreeMap<String, Program>,
    /// `[[channels]]`: named, pinned program sets (`stable`/`nightly`).
    #[serde(default)]
    pub channels: Vec<Channel>,
}

/// One `[programs.<name>]` entry: where the program's release manifests live and how it
/// may be installed.
#[derive(Debug, Clone, Deserialize)]
pub struct Program {
    /// The GitHub repo (under the same account) carrying this program's `pkg-*.toml`.
    pub repo: String,
    /// `"prebuilt-only"` | `"prebuilt-or-build"` (§6). Empty is prebuilt-only: never build
    /// from source without an explicit policy.
    #[serde(default)]
    pub policy: String,
    /// Coherence group: members of one group apply atomically as one tuple (§7). `None` applies
    /// independently (the open-ended R2 tools, and `aterm` itself per §16).
    #[serde(default)]
    pub coherence_group: Option<String>,
    /// `system = "gh"`: a binary of this name on the user's `PATH`, outside the managed `bin/`
    /// and the store, satisfies the program — no download, no shim, status
    /// `satisfied by system: <path>`, reconciled every pass
    /// ([`crate::vendor::system_satisfied`]). When it disappears the program installs through
    /// its artifact. Absent ⇒ always managed here.
    #[serde(default)]
    pub system: Option<String>,
    /// The one-line reason a target with no `[[artifact]]` row cannot have this program
    /// (`"Emacs is a macOS-only member"`), shown verbatim in the
    /// `unavailable on <target>: <hint>` state ([`crate::state::unavailable`]). Absent ⇒ the
    /// generic hint. A missing row is a state, never an error.
    #[serde(default)]
    pub unavailable_hint: Option<String>,
    /// `requires = […]`: retired. Parsed only so a program that names it is refused
    /// ([`Program::names_requires`]) rather than installed ahead of what it names; this client
    /// keeps no install order. The retired `extra` key is not parsed at all: an index carrying
    /// it reads the program as default-set.
    #[serde(default)]
    pub requires: Vec<String>,
}

impl Program {
    /// Whether the signed row names the retired `requires` relation; the install door and the
    /// update pass both refuse such a program ([`crate::FlowError::RequiresRetired`]).
    #[must_use]
    pub fn names_requires(&self) -> bool {
        !self.requires.is_empty()
    }
}

/// One `[[channels]]` entry: a named, pinned set of program builds plus the gating
/// counters.
#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    /// Channel name (`stable`, `nightly`).
    pub name: String,
    /// Monotonic no-downgrade gate for the channel's coherence group.
    #[serde(default)]
    pub channel_build: u64,
    /// Channel-wide yank floor: a pinned build below it is force-upgraded or tombstoned at
    /// apply (§7). It is compared against every program's own build counter, so it is only
    /// meaningful where members share one numbering (the app channel, §16.1). Toolchain members
    /// count independently (`nn = 108` beside `trust-mc = 20065`), so floor one of them through
    /// [`Self::min_build_by_program`].
    #[serde(default)]
    pub min_build: u64,
    /// Per-program yank floors (`program -> floor`), for a channel whose members have
    /// independent build counters. A program's floor is [`Self::min_build_for`]: the max of its
    /// entry and [`Self::min_build`], so an entry can only raise a floor and a client that
    /// predates the key is never stricter than one that knows it.
    #[serde(default)]
    pub min_build_by_program: BTreeMap<String, u64>,
    /// Per-program revocations (`"trust@4790"`), enforced at apply (§7).
    #[serde(default)]
    pub yanked: Vec<String>,
    /// The pinned set: exact per-program builds that move together (`program -> build`).
    #[serde(default)]
    pub pin: BTreeMap<String, u64>,
    /// Per-target pin overlays (`triple -> (program -> build)`), laid over [`Self::pin`] by
    /// [`Self::for_target`], so one target can pin its own build without moving another's. A
    /// client that does not know the key ignores it. Absent ⇒ `for_target` is the identity.
    #[serde(default)]
    pub pin_by_target: BTreeMap<String, BTreeMap<String, u64>>,
}

impl Channel {
    /// The effective yank floor for one program: the max of [`Self::min_build`] and that
    /// program's [`Self::min_build_by_program`] entry.
    ///
    /// Every floor comparison goes through here ([`crate::gate::decide`],
    /// [`crate::gate::current_build_ok`], [`crate::flow::rollback`]'s target predicate): a
    /// build number is comparable only to another build of the same program.
    #[must_use]
    pub fn min_build_for(&self, program: &str) -> u64 {
        self.min_build
            .max(self.min_build_by_program.get(program).copied().unwrap_or(0))
    }

    /// This channel as seen from one target: [`Self::pin`] with `pin_by_target[triple]` laid
    /// over it (an overlay entry replaces that program's pin or adds a program). Every other
    /// field is carried unchanged, and `pin_by_target` is cleared in the view so it cannot be
    /// overlaid twice; other targets' pins neither appear nor mask anything.
    ///
    /// Every consumer that plans, decides or fetches for a host uses this view, so
    /// `plan_groups`, `decide` and `verified_pkg` stay pure over `pin`.
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
    /// The named channel as seen from `triple` ([`Channel::for_target`]), or `None` when the
    /// index has no such channel. The one lookup every host-facing path uses, so no caller
    /// misses a per-target pin.
    #[must_use]
    pub fn channel_for(&self, name: &str, triple: &str) -> Option<Channel> {
        self.channels
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.for_target(triple))
    }

    /// The named program, iff the verified index names it. `None` ⇒ the repo is unreachable
    /// (R4): anything unlisted is never named, so exclusion is by construction.
    #[must_use]
    pub fn program(&self, name: &str) -> Option<&Program> {
        self.programs.get(name)
    }

    /// Whether `name` is a program the index names (§5): an unlisted name is not installable.
    #[must_use]
    pub fn is_program(&self, name: &str) -> bool {
        self.programs.contains_key(name)
    }

    /// The installable set: every program the index names, less the narrowing-only
    /// `[packages].exclude` (R4/§5). No config can make an unlisted repo installable.
    #[must_use]
    pub fn installable(&self, exclude: &[String]) -> BTreeSet<String> {
        let mut set: BTreeSet<String> = self.programs.keys().cloned().collect();
        for e in exclude {
            set.remove(e);
        }
        set
    }
}

/// A machine-signed `pkg-<program>-<build>.toml` (§4.2): the per-triple artifact matrix for
/// one program build.
#[derive(Debug, Clone, Deserialize)]
pub struct PkgManifest {
    /// Manifest format version; `> SUPPORTED_SCHEMA` is refused.
    pub schema: u32,
    /// The program name, which must equal the index `[programs]` key that pointed here. It is
    /// inside the signed bytes, so the caller can refuse a re-pointed program
    /// ([`Self::is_for`]).
    pub program: String,
    /// Human version string (informational / display).
    #[serde(default)]
    pub version: String,
    /// Monotonic build number: the strictly-greater downgrade gate.
    pub build_number: u64,
    /// The binaries to shim into `bin/` (§10); a program may expose several (R2).
    #[serde(default)]
    pub exposes: Vec<String>,
    /// Runtime dependencies: other index-named programs this build needs, each missing one
    /// installed first ([`crate::flow::install`]); a yanked, below-floor, unreachable, unpinned or
    /// cyclic dep is skipped with a warning. An edge never bypasses the floor/yank gate
    /// ([`crate::gate::decide`]) or §5 reachability, and it is signed, so a repo-write adversary
    /// cannot inject one.
    #[serde(default)]
    pub requires: Vec<String>,
    /// `shim_env = ["NAME=VALUE", …]`: the environment every shim of this program (primary and
    /// `alab-` alias) exports before it execs the store binary ([`crate::shim_env`]), so a
    /// managed vendor tool runs with its own updater off (`DISABLE_AUTOUPDATER=1` for Claude
    /// Code) and the index re-pin is its update path. A system copy never runs through the shim.
    /// Validated at parse ([`Reject::ShimEnv`]); read through [`PkgManifest::shim_env`].
    #[serde(default)]
    pub shim_env: Vec<String>,
    /// `[[artifact]]`: one row per target triple. No row for a triple is a clean skip, never
    /// an error (§6).
    #[serde(default, rename = "artifact")]
    pub artifacts: Vec<Artifact>,
}

/// The six target triples a program may carry rows for (§17.1) — the spelling a row's
/// `target` uses and the one the client reports for itself (`cli::current_triple`: a
/// spelling outside this list selects no row, and every install test under it fails).
/// A program with NO row for the running triple is the canonical
/// `unavailable on <target>: <hint>` state, never an error.
pub const TARGETS: &[&str] = &[
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
];

/// The tools the published trust bundle shims (the pkg-trust-6808 set): the producer packs
/// and refuses a seal without each. `ty` left it 2026-08-18 when the standalone `ty` pack took the name: never
/// re-add it — one index exposing a name twice shims it twice.
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

/// One `[[artifact]]` row: how one target triple obtains and applies this build. `kind` is the
/// payload/apply shape and `protocol` where the bytes come from (§17);
/// [`crate::dispatch::strategy_for`] maps the pair to an apply strategy and refuses every pair
/// it does not know.
#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    /// Target triple, one of [`TARGETS`].
    pub target: String,
    /// The payload/apply shape: `binary` | `cargo-src` | `sysroot-bundle` | `app-bundle`
    /// (§4.2/§17). The retired `vendor-fetch` is refused at parse
    /// ([`crate::sig::Reject::RetiredKind`]).
    #[serde(default)]
    pub kind: String,
    /// Where the bytes come from: `github-release` (the default: a release asset under the
    /// account slug) or `https` (a vendor's own download, pinned by this signed row). A row
    /// naming a deleted OS-installer protocol (`pkg`, `system-pm`, `softwareupdate`) parses and
    /// is refused at admission ([`crate::vendor::check_row`]) and dispatch.
    #[serde(default = "default_protocol")]
    pub protocol: String,
    /// `github-release`: the release asset file name to download. `https`: the local staging
    /// file name the download lands under (one bare component). Required for both
    /// ([`parse_pkg`]).
    #[serde(default)]
    pub asset: String,
    /// SHA-256 of the downloaded bytes: the download-integrity gate. Required on every index
    /// row.
    #[serde(default)]
    pub sha256: String,
    /// SHA-512 of the downloaded bytes, for a vendor-direct row whose vendor publishes only
    /// that; empty is absent. A row's gate is the one of `sha256`/`sha512` it sets
    /// ([`Artifact::digest`]); an index row sets `sha256` alone.
    #[serde(default)]
    pub sha512: String,
    /// SHA-256 over the sorted extracted-file list: the apply-time re-verify root (§8
    /// TOCTOU).
    #[serde(default)]
    pub tree_root: String,
    /// Asset size in bytes: drives the per-artifact download cap and disk preflight.
    #[serde(default)]
    pub size: u64,
    /// Relocation policy for a `sysroot-bundle` (§10.1). `self-contained` (the default and the
    /// only one applied): the payload was relocated at pack time, so install extracts and
    /// activates with no rustup on the user side; any other value is refused at apply. Signed,
    /// so a repo-write adversary cannot flip it. Ignored for other kinds.
    #[serde(default = "default_reloc")]
    pub reloc: String,
    /// `[artifact.cost]`: accounting shown before any byte moves (R7).
    #[serde(default)]
    pub cost: Cost,
    /// `https`: the vendor's per-version URL the client downloads from. Signed and narrowed by
    /// the compiled host allow-list ([`crate::vendor::VENDOR_HOSTS`]); the host is a transport,
    /// never an authenticity input — the digest and `tree_root` gate the bytes.
    #[serde(default)]
    pub url: String,
    /// `https` only: how the download is staged: `raw-binary` | `tar-gz-binary` | `tar-gz` |
    /// `tar-zst` | `zip` (each with `kind = "binary"`) | `dmg` (with `kind = "app-bundle"`)
    /// ([`crate::vendor::PAYLOADS`]).
    #[serde(default)]
    pub payload: String,
    /// `raw-binary`/`tar-gz-binary` only: the tool name the download (or the tar's one file)
    /// becomes under `bin/` (mode `0755`), in the platform's executable spelling
    /// ([`crate::store::ToolName::exe_file`]). Must be one of `exposes`.
    #[serde(default)]
    pub entry: String,
    /// Archive payloads only (`tar-gz`/`tar-zst`/`zip`): leading path components to drop
    /// on extraction (`gh_<ver>_macOS_arm64/bin/gh` → `bin/gh`). Default `0`.
    #[serde(default)]
    pub strip_components: u32,
    /// `dmg` (and, if declared, archive) payloads: relative symlinks created at `bin/<name>` →
    /// `../<target>` after staging, so the shims resolve `bin/<tool>`
    /// (`emacs = "Emacs.app/Contents/MacOS/Emacs"`). Every key must be in `exposes`; targets
    /// are relative, `..`-free paths inside the staged tree.
    #[serde(default)]
    pub links: BTreeMap<String, String>,
    /// Display-only vendor name for the consent copy (`"Anthropic PBC"`). Never a trust input.
    #[serde(default)]
    pub vendor: String,
    /// `[artifact.provenance]`: which compiler built this row's payload, as the packer's
    /// lineage scan recorded it ([`crate::lineage`]). Per row, since a merge appends rows packed
    /// on other builders to one manifest. For people and audits; no install decision reads it,
    /// and no struct here denies unknown fields, so older clients ignore it.
    #[serde(default)]
    pub provenance: Option<Provenance>,
}

impl Artifact {
    /// The signed digest that gates this row's download: whichever one of `sha256` and
    /// `sha512` it sets ([`one_digest`]). `None` (which no file matches) when it sets neither
    /// or both.
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
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        }
    }

    /// Whether `hex` is one digest of this algorithm, in lowercase hex.
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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.key())
    }
}

/// The one digest a `sha256`/`sha512` pair names (a signed row's, a log entry's, a record's):
/// `(algorithm, value)` when exactly one is non-empty, `None` otherwise. The value is as
/// written; its shape is the caller's check.
#[must_use]
pub fn one_digest<'a>(sha256: &'a str, sha512: &'a str) -> Option<(DigestAlg, &'a str)> {
    match (sha256.is_empty(), sha512.is_empty()) {
        (false, true) => Some((DigestAlg::Sha256, sha256)),
        (true, false) => Some((DigestAlg::Sha512, sha512)),
        _ => None,
    }
}

/// `[artifact.provenance]`: the scan record of the packer that signed the row. Every
/// field is a string exactly as the packer's payload scan recorded it
/// (`atpkg-publish`'s `artifact::pkgdoc::provenance`).
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
    /// How its lineage was proven: `lineage`, `seed-mint`, or `unproven` (admitted while the
    /// receipt requirement is dormant).
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
    /// Evidence of another compiler it found (always `0` in a signed row): none found, not
    /// none possible (see [`crate::lineage`]).
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
    /// The admitted shim environment ([`crate::shim_env::ShimEnv`]). [`parse_pkg`] already
    /// refused a list that breaks the rule; a hand-built manifest's bad list reads as no
    /// environment.
    #[must_use]
    pub fn shim_env(&self) -> crate::shim_env::ShimEnv {
        crate::shim_env::ShimEnv::admit(&self.shim_env).unwrap_or_default()
    }

    /// The artifact for `target`, if this build ships one. `None` is the clean skip (§6).
    #[must_use]
    pub fn artifact_for(&self, target: &str) -> Option<&Artifact> {
        self.artifacts.iter().find(|a| a.target == target)
    }

    /// Whether the signed `program` field is `name`: the anti-replay bind (§4.2). A manifest
    /// signed for program A must be refused when fetched as B, so fail closed on `false`.
    #[must_use]
    pub fn is_for(&self, name: &str) -> bool {
        self.program == name
    }
}

/// Parse a verified `index.toml`: strict UTF-8 (the signature covers these exact bytes), a real
/// `toml` parse, then the [`SUPPORTED_SCHEMA`] reject-newer gate. Every [`Reject`] means
/// "refuse, install nothing".
///
/// `pub(crate)` on purpose: `machine_id`/`roster_seq` are trustworthy only after
/// `TrustedRoster::authorize_index` binds them to the signing key, and a public parse would let
/// an out-of-crate caller pair `authorize_bytes` with it and read an unbound `machine_id`.
pub(crate) fn parse_index(verified: &VerifiedBytes) -> Result<Index, Reject> {
    let idx: Index = parse_toml(verified)?;
    if idx.schema > SUPPORTED_SCHEMA {
        return Err(Reject::Schema);
    }
    Ok(idx)
}

/// Parse a verified `pkg-*.toml` (the same strict UTF-8 and reject-newer discipline as
/// [`parse_index`]), then its row and `shim_env` rules.
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
/// checked once at parse so no writer meets an entry it cannot embed. A refusal
/// ([`Reject::ShimEnv`], naming the entry) refuses the whole manifest: a program whose declared
/// environment cannot be laid is not installed with another.
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
///
/// Everything finer (hosts, payload lanes, an unknown protocol) is
/// [`crate::vendor::check_row`]'s per-program refusal, so one mis-authored row never takes the
/// other targets' rows down.
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
mod tests {
    use super::*;
    use crate::sig::testkit;

    /// `body`, machine-signed and verified through the real roster chain.
    fn verified(body: &str) -> VerifiedBytes {
        testkit::machine_signed(body.as_bytes().to_vec())
    }

    /// A realistic index naming six programs (and not `dotfiles`, a private-config repo),
    /// attributed to the test roster's machine.
    fn full_index() -> String {
        format!(
            r#"
schema = 2
index_build = 41
generated_at = "2026-06-28T12:00:00Z"
valid_until = "2026-07-05T12:00:00Z"
machine_id = "{id}"
roster_seq = {seq}

[programs.ay]
repo = "ay"
policy = "prebuilt-or-build"

[programs.trust]
repo = "trust"
policy = "prebuilt-only"
coherence_group = "rustc"

[programs.aterm]
repo = "aterm"
policy = "prebuilt-only"

[programs.codex]
repo = "codex"
policy = "prebuilt-only"
extra = true

[programs.vendorx]
repo = "vendorx"
policy = "prebuilt-only"
extra = true

[programs.gh]
repo = "gh"
policy = "prebuilt-only"
system = "gh"

[[channels]]
name = "stable"
channel_build = 137
min_build = 120
yanked = ["trust@4790"]
pin = {{ aterm = 1234, trust = 4821, ay = 18, codex = 2026082601, vendorx = 1, gh = 2026082601 }}

[channels.meta]
nightly = "nightly-2025-12-03"
trust_mc_rev = "0.67.0"
"#,
            id = testkit::MACHINE_ID,
            seq = testkit::SEQ
        )
    }

    /// A per-program floor raises only the program it names; every other program keeps the
    /// channel-wide one.
    #[test]
    fn a_per_program_min_build_floors_one_program_only() {
        let body = format!(
            r#"
schema = 2
index_build = 41
valid_until = "2026-07-05T12:00:00Z"
machine_id = "{id}"
roster_seq = {seq}

[programs.trust]
repo = "trust"
policy = "prebuilt-only"

[programs.nn]
repo = "nn"
policy = "prebuilt-only"

[[channels]]
name = "stable"
channel_build = 137
min_build = 0
min_build_by_program = {{ trust = 7000 }}
pin = {{ trust = 7100, nn = 108 }}
"#,
            id = testkit::MACHINE_ID,
            seq = testkit::SEQ
        );
        let idx = parse_index(&verified(&body)).expect("a per-program floor parses");
        let ch = &idx.channels[0];
        assert_eq!(ch.min_build_for("trust"), 7000, "the floored program");
        assert_eq!(ch.min_build_for("nn"), 0, "a program the floor never named");
        // The floor rides the per-target view like every other field.
        let view = idx
            .channel_for("stable", "aarch64-apple-darwin")
            .expect("stable exists");
        assert_eq!(view.min_build_for("trust"), 7000);
        assert_eq!(view.min_build_for("nn"), 0);
        // Without the key every program keeps the channel-wide floor.
        let old = parse_index(&verified(&full_index())).expect("valid index parses");
        let och = &old.channels[0];
        assert!(och.min_build_by_program.is_empty());
        assert_eq!(och.min_build_for("ay"), och.min_build);
        assert_eq!(och.min_build_for("trust"), och.min_build);
    }

    #[test]
    fn parses_a_full_index_and_its_attribution() {
        let idx = parse_index(&verified(&full_index())).expect("valid index parses");
        assert_eq!(idx.index_build, 41);
        assert_eq!(idx.valid_until, "2026-07-05T12:00:00Z");
        // The attribution pair the roster bind checks.
        assert_eq!(idx.machine_id.as_deref(), Some(testkit::MACHINE_ID));
        assert_eq!(idx.roster_seq, Some(testkit::SEQ));
        // The fixture's `[channels.meta]` table, which no field reads, is ignored.
        assert_eq!(
            idx.program("trust").unwrap().coherence_group.as_deref(),
            Some("rustc")
        );
        assert_eq!(idx.program("ay").unwrap().repo, "ay");
        assert_eq!(idx.program("gh").unwrap().system.as_deref(), Some("gh"));
        assert_eq!(idx.program("ay").unwrap().system, None);
        assert_eq!(idx.channels.len(), 1);
        let ch = &idx.channels[0];
        assert_eq!(ch.name, "stable");
        assert_eq!(ch.pin.get("trust"), Some(&4821));
    }

    /// A per-target overlay changes only the target it names: that target sees the overlaid
    /// build for a program `pin` also names (replace) and a program `pin` never named (add);
    /// every other target sees `pin` as published.
    #[test]
    fn a_per_target_pin_overlays_its_own_target_and_no_other() {
        let mut body = full_index();
        body = body.replace(
            "[channels.meta]",
            "pin_by_target = { \"x86_64-unknown-linux-gnu\" = { trust = 9122, nn = 108 } }\n[channels.meta]",
        );
        let idx = parse_index(&verified(&body)).expect("an index with a per-target overlay parses");
        let raw = &idx.channels[0];
        assert_eq!(
            raw.pin.get("trust"),
            Some(&4821),
            "the platform-agnostic pin is untouched on the wire"
        );

        let linux = idx
            .channel_for("stable", "x86_64-unknown-linux-gnu")
            .expect("stable exists");
        assert_eq!(
            linux.pin.get("trust"),
            Some(&9122),
            "REPLACED for the overlaid target"
        );
        assert_eq!(
            linux.pin.get("nn"),
            Some(&108),
            "ADDED: a program `pin` never named"
        );
        assert_eq!(
            linux.pin.get("ay"),
            Some(&18),
            "carried: a program the overlay does not mention"
        );
        assert!(
            linux.pin_by_target.is_empty(),
            "the view cannot be overlaid twice"
        );
        assert_eq!(linux.name, "stable");
        assert_eq!(
            linux.min_build, raw.min_build,
            "every other field rides along"
        );

        let mac = idx
            .channel_for("stable", "aarch64-apple-darwin")
            .expect("stable exists");
        assert_eq!(
            mac.pin.get("trust"),
            Some(&4821),
            "another target sees the published pin"
        );
        assert_eq!(
            mac.pin.get("nn"),
            None,
            "and not the other target's additions"
        );
        assert!(
            idx.channel_for("nightly", "x86_64-unknown-linux-gnu")
                .is_none(),
            "no such channel"
        );
    }

    /// The builds clients resolve, over every triple an index names, are `pin` ∪ every
    /// `pin_by_target` overlay: the set the public mirror's work list (`atpkg-publish mirror`)
    /// must carry. Neither half suffices: an overlay build appears in no `pin` row, and the
    /// `pin` build an overlay masks is still resolved by every other triple.
    #[test]
    fn the_builds_clients_resolve_are_pin_union_every_overlay() {
        use std::collections::BTreeSet;

        let body = full_index().replace(
            "[channels.meta]",
            "pin_by_target = { \"x86_64-unknown-linux-gnu\" = { trust = 9122, nn = 108 }, \
             \"aarch64-unknown-linux-gnu\" = { trust = 9123 } }\n[channels.meta]",
        );
        let idx = parse_index(&verified(&body)).expect("an index with two overlays parses");
        let ch = &idx.channels[0];

        // The named triples: `pin` serves every target, each overlay key one more.
        let mut triples: BTreeSet<&str> = ch.pin_by_target.keys().map(String::as_str).collect();
        assert_eq!(triples.len(), 2, "the fixture names two overlaid triples");
        triples.insert("aarch64-apple-darwin"); // a target no overlay names

        // What some client somewhere actually resolves.
        let resolvable: BTreeSet<(String, u64)> = triples
            .iter()
            .flat_map(|t| {
                idx.channel_for(&ch.name, t)
                    .expect("stable exists")
                    .pin
                    .into_iter()
            })
            .collect();

        // What a publisher reading the two keys would carry.
        let declared: BTreeSet<(String, u64)> = ch
            .pin
            .iter()
            .map(|(p, b)| (p.clone(), *b))
            .chain(
                ch.pin_by_target
                    .values()
                    .flat_map(|o| o.iter().map(|(p, b)| (p.clone(), *b))),
            )
            .collect();

        assert_eq!(
            resolvable, declared,
            "the resolvable set IS `pin` ∪ every overlay — if this stops holding, \
             the public mirror's work list no longer describes what it has to carry"
        );

        // Neither key alone covers it.
        let pin_only: BTreeSet<(String, u64)> =
            ch.pin.iter().map(|(p, b)| (p.clone(), *b)).collect();
        assert!(
            resolvable.contains(&("trust".to_string(), 9122))
                && resolvable.contains(&("nn".to_string(), 108))
                && !pin_only.contains(&("trust".to_string(), 9122))
                && !pin_only.contains(&("nn".to_string(), 108)),
            "an overlay build is resolvable and appears in no `pin` row"
        );
        assert!(
            resolvable.contains(&("trust".to_string(), 4821)),
            "the `pin` build an overlay masks is still resolved by every other target, \
             so a publisher may never drop it in favour of the overlay"
        );
    }

    // R4: a private-config repo (never named in the index) is unreachable BY CONSTRUCTION.
    #[test]
    fn unlisted_repo_is_unreachable() {
        let idx = parse_index(&verified(&full_index())).unwrap();
        assert!(
            idx.program("dotfiles").is_none(),
            "an unlisted repo is not a program"
        );
        assert!(!idx.is_program("dotfiles"));
        assert!(
            !idx.installable(&[]).contains("dotfiles"),
            "a private-config repo can never be in the installable set"
        );
        // The named programs are reachable.
        assert!(idx.is_program("trust") && idx.is_program("ay") && idx.is_program("aterm"));
    }

    // §5: `exclude` only narrows.
    #[test]
    fn exclude_is_narrowing_only() {
        let idx = parse_index(&verified(&full_index())).unwrap();
        // No exclude: every named program (`gh` included — `system` is a satisfaction
        // rule, not a tier).
        assert_eq!(
            idx.installable(&[]),
            ["aterm", "ay", "codex", "gh", "trust", "vendorx"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
        // exclude subtracts.
        assert_eq!(
            idx.installable(&["trust".into(), "gh".into()]),
            ["aterm", "ay", "codex", "vendorx"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
    }

    /// The `https` row keys parse as signed data (all `serde(default)`, so a row without
    /// them — every existing manifest — is unchanged), `links` is a map, and a row with no
    /// `protocol` is `github-release`.
    #[test]
    fn parses_https_protocol_artifact_keys() {
        let body = r#"
schema = 2
program = "claude"
version = "2.1.231"
build_number = 2026082601
exposes = ["claude"]

[[artifact]]
target = "aarch64-apple-darwin"
kind = "binary"
protocol = "https"
url = "https://downloads.claude.ai/claude-code-releases/2.1.231/darwin-arm64/claude"
payload = "raw-binary"
entry = "claude"
asset = "claude-2.1.231-darwin-arm64"
sha256 = "7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c"
tree_root = "abc"
size = 230824016
vendor = "Anthropic PBC"
strip_components = 1
links = { emacs = "Emacs.app/Contents/MacOS/Emacs" }
"#;
        let m = parse_pkg(&verified(body)).expect("valid https manifest");
        let a = m.artifact_for("aarch64-apple-darwin").unwrap();
        assert_eq!(a.kind, "binary");
        assert_eq!(a.protocol, "https");
        assert!(a.url.starts_with("https://downloads.claude.ai/"));
        assert_eq!(a.payload, "raw-binary");
        assert_eq!(a.entry, "claude");
        assert_eq!(a.vendor, "Anthropic PBC");
        assert_eq!(a.strip_components, 1);
        assert_eq!(
            a.links.get("emacs").map(String::as_str),
            Some("Emacs.app/Contents/MacOS/Emacs")
        );
        // Absent keys default to empty or zero, and the undeclared protocol is
        // `github-release`.
        let plain = parse_pkg(&verified(
            "schema = 2\nprogram = \"ay\"\nbuild_number = 18\n[[artifact]]\n\
             target = \"aarch64-apple-darwin\"\nasset = \"ay-18.tar.zst\"\nsha256 = \"d\"\n",
        ))
        .unwrap();
        let a = plain.artifact_for("aarch64-apple-darwin").unwrap();
        assert_eq!(a.protocol, "github-release");
        assert!(a.url.is_empty() && a.payload.is_empty() && a.entry.is_empty());
        assert_eq!(a.strip_components, 0);
        assert!(a.links.is_empty() && a.vendor.is_empty());
    }

    /// `shim_env` is validated at parse: the accepted shape reads back through
    /// `PkgManifest::shim_env`, an absent key is no environment, and any refused entry refuses
    /// the whole manifest as `Reject::ShimEnv` naming it, never `Malformed`.
    #[test]
    fn shim_env_is_signed_validated_and_refused_by_name() {
        let head = "schema = 2\nprogram = \"claude\"\nversion = \"2.1.231\"\n\
                    build_number = 2026082601\nexposes = [\"claude\"]\n";
        let row = "[[artifact]]\ntarget = \"aarch64-apple-darwin\"\nkind = \"binary\"\n\
                   protocol = \"https\"\nurl = \"https://downloads.claude.ai/c/claude\"\n\
                   payload = \"raw-binary\"\nentry = \"claude\"\nasset = \"claude-arm64\"\n\
                   sha256 = \"d\"\ntree_root = \"abc\"\nsize = 1\n";
        let with = format!("{head}shim_env = [\"DISABLE_AUTOUPDATER=1\"]\n{row}");
        let m = parse_pkg(&verified(&with)).expect("a valid shim_env parses");
        assert_eq!(m.shim_env, vec!["DISABLE_AUTOUPDATER=1".to_string()]);
        assert_eq!(m.shim_env().spelled(), "DISABLE_AUTOUPDATER=1");
        // Absent: no environment, exactly as every manifest published before the key.
        let without = parse_pkg(&verified(&format!("{head}{row}"))).unwrap();
        assert!(without.shim_env.is_empty() && without.shim_env().is_empty());
        // Refused, each by name, as ShimEnv — the publisher's verify-pkg spells the fix.
        // A control byte inside the TOML string (the string escape carries it through the
        // parser) is refused the same way.
        for list in [
            "[\"DISABLE_AUTOUPDATER\"]",
            "[\"disable_autoupdater=1\"]",
            "[\"PATH=/tmp\"]",
            "[\"X=\"]",
            "[\"X=1\", \"X=2\"]",
            "[\"A=1\", \"B=1\", \"C=1\", \"D=1\", \"E=1\", \"F=1\", \"G=1\", \"H=1\", \"I=1\"]",
            "[\"X=a\\nb\"]",
        ] {
            let body = format!("{head}shim_env = {list}\n{row}");
            match parse_pkg(&verified(&body)) {
                Err(Reject::ShimEnv(_)) => {}
                other => panic!("{list}: expected Reject::ShimEnv, got {other:?}"),
            }
        }
        // The wrong TYPE is the parser's own refusal, as for any key.
        let body = format!("{head}shim_env = \"DISABLE_AUTOUPDATER=1\"\n{row}");
        assert_eq!(parse_pkg(&verified(&body)).unwrap_err(), Reject::Malformed);
    }

    /// The retired index-level `requires` still parses, whatever it names (a cycle, an unlisted
    /// name), so the program naming it is refused by name rather than the whole index.
    #[test]
    fn the_retired_requires_parses_so_its_program_can_be_refused() {
        let body = full_index()
            .replace(
                "[programs.gh]\nrepo = \"gh\"",
                "[programs.gh]\nrequires = [\"ay\", \"dotfiles\"]\nrepo = \"gh\"",
            )
            .replace(
                "[programs.ay]\nrepo = \"ay\"",
                "[programs.ay]\nrequires = [\"gh\"]\nrepo = \"ay\"",
            );
        let idx = parse_index(&verified(&body)).expect("an index naming `requires` verifies");
        assert!(idx.program("gh").unwrap().names_requires());
        assert!(idx.program("ay").unwrap().names_requires());
        assert!(!idx.program("trust").unwrap().names_requires());
    }

    /// The retired `kind = "vendor-fetch"` is refused at parse with the split spelled out
    /// — never read as a binary, never as an https row, never silently Unknown.
    #[test]
    fn the_retired_vendor_fetch_kind_is_refused_naming_the_split() {
        let body = "schema = 2\nprogram = \"claude\"\nbuild_number = 1\n[[artifact]]\n\
                    target = \"aarch64-apple-darwin\"\nkind = \"vendor-fetch\"\n\
                    asset = \"claude\"\nsha256 = \"d\"\nurl = \"https://downloads.claude.ai/c\"\n";
        match parse_pkg(&verified(body)) {
            Err(Reject::RetiredKind(why)) => assert!(why.contains("vendor-fetch"), "{why}"),
            other => panic!("expected RetiredKind, got {other:?}"),
        }
        // Even a second row spelled that way refuses the whole manifest.
        let mixed = "schema = 2\nprogram = \"claude\"\nbuild_number = 1\n[[artifact]]\n\
                     target = \"aarch64-apple-darwin\"\nasset = \"a\"\nsha256 = \"d\"\n\
                     [[artifact]]\ntarget = \"x86_64-apple-darwin\"\nkind = \"vendor-fetch\"\n\
                     asset = \"a\"\nsha256 = \"d\"\n";
        assert!(matches!(
            parse_pkg(&verified(mixed)),
            Err(Reject::RetiredKind(_))
        ));
    }

    /// A row of a deleted protocol (`pkg`, `system-pm`) still parses, so admission refuses that
    /// one program by name instead of the whole manifest being malformed; a release-shaped row
    /// still fails closed without `asset`/`sha256`.
    #[test]
    fn deleted_protocol_rows_parse_and_release_rows_still_need_their_digests() {
        let body = r#"
schema = 2
program = "homebrew"
version = "4.5.0"
build_number = 2026082701
exposes = []

[[artifact]]
target = "aarch64-apple-darwin"
kind = "installer-pkg"
protocol = "pkg"
url = "https://github.com/Homebrew/brew/releases/download/4.5.0/Homebrew-4.5.0.pkg"
sha256 = "7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c7b09f01c"
size = 144434507
signer_team = "927JGANW46"
elevated = true
provides = ["/opt/homebrew/bin/brew"]
vendor = "Homebrew"

[[artifact]]
target = "x86_64-unknown-linux-gnu"
kind = "system-package"
protocol = "system-pm"
manager = "apt"
package = "emacs"
provides = ["emacs", "/usr/bin/emacs"]
elevated = true

[[artifact]]
target = "x86_64-pc-windows-msvc"
kind = "system-package"
protocol = "system-pm"
manager = "winget"
package = "GNU.Emacs"
provides = ["emacs"]
"#;
        let m = parse_pkg(&verified(body)).expect("pkg + system-pm rows parse");
        let p = m.artifact_for("aarch64-apple-darwin").unwrap();
        assert_eq!(
            (p.kind.as_str(), p.protocol.as_str()),
            ("installer-pkg", "pkg")
        );
        assert!(matches!(
            crate::vendor::check_row(p, &m.exposes),
            Err(crate::FlowError::VendorRefused(_))
        ));
        let apt = m.artifact_for("x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(
            (apt.kind.as_str(), apt.protocol.as_str()),
            ("system-package", "system-pm")
        );
        // A target with no row is the clean skip, never an error.
        assert!(m.artifact_for("aarch64-pc-windows-msvc").is_none());
        // Release-shaped rows still require their digests, and an index row's digest is its
        // sha256 alone.
        for missing in [
            "target = \"aarch64-apple-darwin\"\nsha256 = \"d\"\n",
            "target = \"aarch64-apple-darwin\"\nasset = \"a\"\n",
            "target = \"aarch64-apple-darwin\"\nasset = \"a\"\nsha512 = \"d\"\n",
            "target = \"aarch64-apple-darwin\"\nasset = \"a\"\nsha256 = \"d\"\nsha512 = \"d\"\n",
            "target = \"aarch64-apple-darwin\"\nprotocol = \"https\"\nkind = \"binary\"\n\
             sha256 = \"d\"\nurl = \"https://github.com/x\"\n",
        ] {
            let body =
                format!("schema = 2\nprogram = \"ay\"\nbuild_number = 18\n[[artifact]]\n{missing}");
            assert_eq!(
                parse_pkg(&verified(&body)).unwrap_err(),
                Reject::Malformed,
                "{missing}"
            );
        }
    }

    /// A row's gate is the one digest it sets, named by its algorithm; neither or both is no
    /// digest. The shape check takes each algorithm's length, lowercase only (the sample is a
    /// real vendor's published sha512).
    #[test]
    fn a_row_is_gated_by_the_one_digest_it_sets() {
        const AGY_SHA512: &str = "473a34b685d0a09d48790e8b86bc3e545dfab889fe0d8fc81cb7e1fdfc336fe7\
                                  8ce57022e3b438c754c93c9cffeb13fdbd177e50fdedc087d52732fa5475183f";
        let sha256 = "7b09f01c".repeat(8);
        assert_eq!(
            one_digest(&sha256, ""),
            Some((DigestAlg::Sha256, sha256.as_str()))
        );
        assert_eq!(
            one_digest("", AGY_SHA512),
            Some((DigestAlg::Sha512, AGY_SHA512))
        );
        assert_eq!(
            one_digest(&sha256, AGY_SHA512),
            None,
            "two digests are no gate"
        );
        assert_eq!(one_digest("", ""), None);
        assert!(DigestAlg::Sha512.is_lower_hex(AGY_SHA512));
        assert!(DigestAlg::Sha256.is_lower_hex(&sha256));
        for (alg, bad) in [
            (DigestAlg::Sha256, AGY_SHA512.to_string()),
            (DigestAlg::Sha512, sha256.clone()),
            (DigestAlg::Sha512, AGY_SHA512.to_uppercase()),
            (DigestAlg::Sha512, AGY_SHA512.replacen('4', "g", 1)),
        ] {
            assert!(!alg.is_lower_hex(&bad), "{alg} {bad}");
        }
    }

    /// Every published triple is a forge cell, so some lane compiles it.
    #[test]
    fn every_published_triple_is_a_forge_cell() {
        let cells = aterm_forge::resolve::default_cells();
        for t in TARGETS {
            assert!(
                cells.iter().any(|c| c.triple == *t),
                "no forge cell compiles `{t}`, and atpkg publishes artifact rows for it"
            );
        }
    }

    // reject-newer: a schema beyond this build is refused (the client stays put).
    #[test]
    fn rejects_newer_schema() {
        let body = full_index().replace("schema = 2", "schema = 99");
        assert_eq!(parse_index(&verified(&body)).unwrap_err(), Reject::Schema);
        // The current schema is accepted: the gate is the number, not the fixture.
        assert!(parse_index(&verified(&full_index())).is_ok());
    }

    /// A schema-1 index still parses (1 ≤ `SUPPORTED_SCHEMA`) but carries no attribution, so
    /// the roster bind refuses it.
    #[test]
    fn a_schema_one_index_parses_but_carries_no_attribution() {
        let body = "schema = 1\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
                    [keys]\nrelease_key_id = \"rk\"\nrelease_key_pubkey = \"AAAA\"\n";
        let idx = parse_index(&verified(body)).expect("an older schema is not a parse failure");
        assert_eq!(idx.machine_id, None);
        assert_eq!(idx.roster_seq, None);
        // The bind is what refuses it; `sig`'s tests prove that direction end to end.
    }

    /// Each fails closed as `Malformed`, never a silent default: a missing `index_build`; a
    /// duplicate `machine_id`, which a last-wins parser would let re-attribute the index; a
    /// string `roster_seq`, which must not read as absent.
    #[test]
    fn malformed_indexes_fail_closed() {
        for (body, why) in [
            (
                "schema = 2\nvalid_until = \"2026-07-05T12:00:00Z\"\n",
                "missing index_build",
            ),
            (
                "schema = 2\nindex_build = 1\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
                 machine_id = \"m3\"\nmachine_id = \"m11\"\nroster_seq = 3\n",
                "duplicate machine_id",
            ),
            (
                "schema = 2\nindex_build = 1\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
                 machine_id = \"m3\"\nroster_seq = \"3\"\n",
                "string roster_seq",
            ),
        ] {
            assert_eq!(
                parse_index(&verified(body)).unwrap_err(),
                Reject::Malformed,
                "{why}"
            );
        }
    }

    // Table scoping is intrinsic to the real parser: a `machine_id` in a sibling table lands
    // in an ignored table and cannot shadow the top-level attribution.
    #[test]
    fn sibling_table_cannot_hijack_the_attribution() {
        let body = "schema = 2\nindex_build = 1\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
                    machine_id = \"m3\"\nroster_seq = 3\n\
                    [meta]\nmachine_id = \"m11\"\nroster_seq = 9\n";
        let idx = parse_index(&verified(body)).expect("parses; [meta] ignored");
        assert_eq!(idx.machine_id.as_deref(), Some("m3"));
        assert_eq!(idx.roster_seq, Some(3));
    }

    // `pkg-*.toml`: per-triple artifact selection, and a clean skip for a missing triple.
    #[test]
    fn parses_pkg_manifest_and_selects_artifact() {
        let body = r#"
schema = 2
program = "trust"
version = "1.96.0-dev"
build_number = 4821
exposes = ["trust", "trust-mc"]

[[artifact]]
target = "aarch64-apple-darwin"
kind = "sysroot-bundle"
asset = "trust-4821-aarch64-apple-darwin.tar.zst"
sha256 = "deadbeef"
size = 1837465600
[artifact.cost]
download_bytes = 1837465600
disk_installed = 3221225472
build_seconds = 0
"#;
        let m = parse_pkg(&verified(body)).expect("valid pkg manifest");
        assert_eq!(m.program, "trust");
        assert_eq!(m.build_number, 4821);
        assert_eq!(m.exposes, vec!["trust".to_string(), "trust-mc".to_string()]);
        let a = m
            .artifact_for("aarch64-apple-darwin")
            .expect("triple present");
        assert_eq!(a.sha256, "deadbeef");
        assert_eq!(a.cost.disk_installed, 3221225472);
        // A triple with no row is a clean fail-closed skip, not an error.
        assert!(m.artifact_for("x86_64-unknown-linux-gnu").is_none());
    }

    // Strict UTF-8: an invalid byte inside genuinely verified bytes is `Malformed`, never
    // U+FFFD-substituted (the parse layer's check, not the signature's).
    #[test]
    fn strict_utf8_rejects_invalid_bytes_in_signed_index() {
        let mut raw = b"schema = 2\nindex_build = 1\n".to_vec();
        raw.push(0xFF); // not valid UTF-8
        raw.extend_from_slice(b"\nvalid_until = \"2026-07-05T12:00:00Z\"\n");
        assert_eq!(
            parse_index(&testkit::machine_signed(raw)).unwrap_err(),
            Reject::Malformed
        );
    }

    // `parse_pkg` refuses a newer schema and fails closed on a missing required field.
    #[test]
    fn parse_pkg_rejects_newer_schema_and_missing_field() {
        let newer = "schema = 99\nprogram = \"ay\"\nbuild_number = 1\n";
        assert_eq!(parse_pkg(&verified(newer)).unwrap_err(), Reject::Schema);
        let missing = "schema = 2\nprogram = \"ay\"\n";
        assert_eq!(
            parse_pkg(&verified(missing)).unwrap_err(),
            Reject::Malformed
        );
    }

    /// `[artifact.provenance]` rides per row, exactly as `atpkg-publish` writes it
    /// (after the row's `[artifact.cost]`, with a merged row appended after it).
    #[test]
    fn provenance_rides_per_row() {
        let commit = "7d5533b2bb9c9417bc09c27f1acd9d0b808f8f15";
        let body = format!(
            "schema = 1\nprogram = \"ay\"\nversion = \"0.28.0\"\nbuild_number = 9001\nexposes = [\"ay\"]\n\n\
             [[artifact]]\ntarget = \"aarch64-apple-darwin\"\nkind = \"binary\"\nprotocol = \"github-release\"\n\
             asset = \"ay-9001.tar.zst\"\nsha256 = \"{h}\"\ntree_root = \"{h}\"\nsize = 10\n\
             [artifact.cost]\ndownload_bytes = 10\ndisk_installed = 20\nbuild_seconds = 0\n\
             [artifact.provenance]\ncompiler_release = \"1.99.0-dev\"\ncompiler_commit = \"{commit}\"\n\
             lineage = \"unproven\"\nscan_objects = \"5\"\nscan_findings = \"0\"\n\n\
             [[artifact]]\ntarget = \"x86_64-apple-darwin\"\nkind = \"binary\"\nprotocol = \"github-release\"\n\
             asset = \"ay-9001-x86_64-apple-darwin.tar.zst\"\nsha256 = \"{h}\"\nsize = 10\n\
             [artifact.cost]\ndownload_bytes = 10\ndisk_installed = 20\nbuild_seconds = 0\n",
            h = "a".repeat(64)
        );
        let m = parse_pkg(&verified(&body)).expect("a manifest carrying provenance parses");
        let p = m.artifacts[0]
            .provenance
            .as_ref()
            .expect("row 0 carries provenance");
        assert_eq!(p.compiler_commit, commit);
        assert_eq!(p.lineage, "unproven");
        assert_eq!(p.scan_findings, "0");
        assert_eq!(
            m.artifacts[1].provenance, None,
            "a merged row without the table has none"
        );
        assert_eq!(
            m.artifacts[1].cost.disk_installed, 20,
            "the table does not swallow the next row"
        );
    }

    // The signed `program` field binds a manifest to its program (§4.2 anti-replay): a pkg
    // signed for "ay" is refused when fetched as another program.
    #[test]
    fn pkg_program_field_binds_identity() {
        let m = parse_pkg(&verified(
            "schema = 2\nprogram = \"ay\"\nbuild_number = 18\n",
        ))
        .unwrap();
        assert!(m.is_for("ay"));
        assert!(
            !m.is_for("trust"),
            "a pkg signed for ay must not pass as trust"
        );
    }
}
