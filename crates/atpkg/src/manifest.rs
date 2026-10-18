// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The signed manifest schemas (§4) and the discovery allow-list (§5), parsed
//! **only after signature verification**.
//!
//! Two TOML document types, each verified as exact raw bytes by [`crate::sig`] before
//! a single byte reaches a parser here:
//!
//! * [`Index`] — the MACHINE-signed `index.toml`: the freshness anchor, the attribution
//!   pair (`machine_id` / `roster_seq`) that binds it to the roster generation which
//!   authorized it, the **allow-list** of installable programs, and the channels. A repo
//!   NOT named in `[programs]` is unreachable **by construction** (R4) — private-config
//!   repos are excluded because they are never named.
//! * [`PkgManifest`] — a `pkg-<program>-<build>.toml` signed by a machine on that same
//!   roster: the per-triple artifact table, the `exposes` shim list, and the honest
//!   `[cost]`.
//!
//! Every parse *entry point* here ([`parse_index`] / [`parse_pkg`]) takes
//! `&`[`VerifiedBytes`] (which has no public constructor), so the crate's own parse path
//! cannot run on unverified input — the same compile-time guarantee the line-scan stopgap
//! had, scoped to these functions (the schema structs derive `Deserialize` so a *caller*
//! can read the parsed result; that derive is an internal detail, not a sanctioned
//! unverified-parse API). It runs over the **real `toml` parser**: duplicate keys are a
//! hard error and
//! table scoping is intrinsic, so the line-scanner/real-TOML differential the Phase-1
//! `parse_delegation` had to hand-defend against simply cannot arise. Both carry the
//! `SUPPORTED_SCHEMA` **reject-newer** gate (a manifest from a newer format this build
//! cannot safely interpret is refused, fail-closed, rather than misread).

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::sig::{Reject, VerifiedBytes};

/// The highest manifest `schema` this build understands. A document declaring a higher
/// schema is from a newer format we cannot safely interpret, so it is **rejected** (the
/// client stays put) rather than misread — mirrors `aterm-update`'s `SUPPORTED_SCHEMA`.
///
/// # Why this went 1 → 2
///
/// Schema 1's `index.toml` carried a `[keys]` table naming the rotatable release key that
/// signed each `pkg-*.toml`: the index WAS the delegation. Under the single root that
/// authority moved to the master-signed machine roster, so `[keys]` is retired and two
/// attribution fields (`machine_id`, `roster_seq`) take its place.
///
/// The bump is not decoration. It is what makes the transition legible in both
/// directions:
///
/// * a schema-1 client meeting a schema-2 index refuses it here (`Reject::Schema`) and
///   says "newer format" rather than parsing a document whose `[keys]` absence it would
///   read as malformed — and it would have refused anyway, because a machine-signed index
///   cannot verify under its retired root key;
/// * a schema-2 client meeting a schema-1 index parses it (1 ≤ 2) but refuses it at the
///   attribution bind, because a schema-1 index carries no `machine_id`
///   ([`Reject::Unattributed`]) — and, again, would already have failed the signature.
///
/// Both directions fail CLOSED, twice over. There is no migration tier: a client from
/// before the fold (installs older than v0.21.0) installs nothing new until reinstalled.
pub const SUPPORTED_SCHEMA: u32 = 2;

/// The default repository the signed index lives on, under the configurable account:
/// `github.com/<account>/aterm` (§5). The index is a small signed release asset on the
/// **aterm repo itself** — no dedicated repo to administer, 1-to-1 with the existing
/// repos, and coherent with §16 (aterm is itself an index member). Not overridable: the
/// `ATPKG_INDEX_REPO` override is gone (2026-09-23; [`crate::discovery::index_repo`]).
pub const INDEX_REPO: &str = "aterm";

/// The root-signed `index.toml` (§4.1): allow-list + key delegation + freshness +
/// channels. Unknown top-level keys/tables are ignored (forward-compatible within a
/// schema); a *newer* schema is rejected by [`parse_index`].
#[derive(Debug, Clone, Deserialize)]
pub struct Index {
    /// Manifest format version; `> SUPPORTED_SCHEMA` is refused.
    pub schema: u32,
    /// Monotonic index counter — the durable high-water rollback floor (§8). Required:
    /// an index without one is malformed and fails closed.
    pub index_build: u64,
    /// RFC3339 generation time (informational).
    #[serde(default)]
    pub generated_at: String,
    /// RFC3339 freshness deadline; the client refuses this index at/after it (§8). The
    /// freshness comparison itself is done by the caller via [`crate::sig::check_freshness`].
    pub valid_until: String,
    /// WHICH MACHINE on the roster cut this index — the attribution half of the id bind
    /// ([`aterm_update_core::roster::Attribution::bind`]).
    ///
    /// It sits INSIDE the signed bytes, which is what makes the bind free and two-way: a
    /// genuine m3 signature cannot be relabelled m11 (the bytes, and so the signature,
    /// would change), and a thief holding m11's key cannot claim `machine_id = "m3"`
    /// (the roster maps m3 to m3's key, and the verification ran against m11's).
    ///
    /// `Option` because serde must be able to PARSE a document that lacks it — a schema-1
    /// index, or a hand-written one. Absent is a REFUSAL under an armed anchor
    /// ([`crate::sig::Reject::Unattributed`]), never a pass: an index nobody can be held
    /// to is not an index this client installs from.
    #[serde(default)]
    pub machine_id: Option<String>,
    /// The roster generation that authorized the machine which signed this index. Bound
    /// to the roster actually used, so an old roster cannot be paired with a new index; a
    /// NEWER roster with an older index is admitted (the roster travels on the channel
    /// head). Absent ⇒ [`crate::sig::Reject::SeqMismatch`].
    #[serde(default)]
    pub roster_seq: Option<u64>,
    /// `[programs.<name>]` — the open-ended allow-list. The map key is the program name
    /// (`exposes`/install identity); the value names its repo + policy + optional group.
    #[serde(default)]
    pub programs: BTreeMap<String, Program>,
    /// `[[channels]]` — named, pinned program sets (`stable`/`nightly`). Parsed here;
    /// the coherence-group apply semantics land in Phase 4.
    #[serde(default)]
    pub channels: Vec<Channel>,
}

// `[keys]` — the schema-1 release-key delegation — is GONE, along with the `Keys` struct
// and `Index::delegation()` that fed `sig::verify_pkg`. The roster now supplies both the
// grant and the deny for `pkg-*.toml`, which is the whole "one root, one revocation
// story" decision; see `crate::sig`. Unknown top-level tables are ignored, so a producer
// that still emits `[keys]` during the changeover is not refused for it — the table is
// simply no longer read by anything, and carries no authority.

/// One `[programs.<name>]` entry: where the program's release manifests live and how it
/// may be installed.
#[derive(Debug, Clone, Deserialize)]
pub struct Program {
    /// The GitHub repo (under the same account) carrying this program's `pkg-*.toml`.
    pub repo: String,
    /// `"prebuilt-only"` | `"prebuilt-or-build"` (§6). Empty ⇒ treated as prebuilt-only
    /// by later phases (fail-closed: never build from source without an explicit policy).
    #[serde(default)]
    pub policy: String,
    /// Coherence group: members of the same group apply atomically as one tuple (§7).
    /// `None` ⇒ loosely-coupled, applies independently (the open-ended R2 tools, and
    /// `aterm` itself per §16).
    #[serde(default)]
    pub coherence_group: Option<String>,
    /// `system = "gh"` ⇒ a binary of this name already on the user's `PATH` — OUTSIDE the
    /// managed `bin/` and the atpkg store — SATISFIES the program: no download, no shim,
    /// status `satisfied by system: <path>`, reconciled on every pass
    /// ([`crate::vendor::system_satisfied`]). If it disappears the program installs through
    /// its artifact like any other member. Absent ⇒ the program is always managed here.
    #[serde(default)]
    pub system: Option<String>,
    /// The one-line reason a target this program carries NO `[[artifact]]` row for cannot
    /// have it (`"Emacs is a macOS-only member"`), surfaced verbatim in the canonical
    /// `unavailable on <target>: <hint>` state ([`crate::state::unavailable`]). Absent ⇒
    /// the generic hint. A missing row is a STATE, never an error, with or without one.
    #[serde(default)]
    pub unavailable_hint: Option<String>,
    /// `requires = […]` — RETIRED 2026-09-24 with the OS-installer protocols it ordered
    /// (design 2026-09-22 §5.3(b)). Parsed only so a program that names it is REFUSED
    /// ([`Program::names_requires`]) rather than installed ahead of what it names; this
    /// client keeps no install order. `extra`, retired the same day (§5.3(c)), is not
    /// parsed at all: an older index that still carries it verifies and reads the program
    /// as default-set. Absent ⇒ none.
    #[serde(default)]
    pub requires: Vec<String>,
}

impl Program {
    /// Whether the signed row names the retired `requires` relation — the one question
    /// the install door and the update pass ask before a member of such a program moves
    /// (both refuse it, [`crate::FlowError::RequiresRetired`]).
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
    /// CHANNEL-WIDE yank floor: a pinned build below this is force-upgraded / tombstoned at
    /// apply (§7). It is compared against EVERY program's OWN build counter, so it is only
    /// meaningful on a channel whose members share one numbering — the app channel (§16.1),
    /// where the only member is `aterm`. The toolchain channel's members count
    /// independently (the live index pins `nn = 108` beside `trust-mc = 20065`), so a scalar
    /// floor raised to revoke ONE program's builds would tombstone every program numbered
    /// below it; floor those through [`Self::min_build_by_program`], which is per program.
    #[serde(default)]
    pub min_build: u64,
    /// PER-PROGRAM yank floors (`program -> floor`) — the floor to use on a channel whose
    /// members have INDEPENDENT build counters. A program's effective floor is
    /// [`Self::min_build_for`]: the MAX of its entry here and the channel-wide
    /// [`Self::min_build`]. Max, not override, so this key can only ever RAISE a floor —
    /// an entry can never lower a program below the channel-wide floor an older client
    /// already enforces, and a client that predates the key is never stricter than one that
    /// knows it. Absent ⇒ every program's floor is the channel-wide one (the old
    /// behaviour, byte for byte).
    #[serde(default)]
    pub min_build_by_program: BTreeMap<String, u64>,
    /// Per-program revocations (`"trust@4790"`), enforced at apply (§7).
    #[serde(default)]
    pub yanked: Vec<String>,
    /// The pinned SET — exact per-program builds that move together (`program -> build`).
    #[serde(default)]
    pub pin: BTreeMap<String, u64>,
    /// PER-TARGET pin overlays (`triple -> (program -> build)`), applied over [`Self::pin`]
    /// by [`Self::for_target`]. A build number is platform-agnostic in `pin`, which forces
    /// every target to move on one cadence: a Linux toolchain could only be published by
    /// riding the Mac's build number — and the Mac's exact source for a given number is
    /// not always recoverable (measured 2026-09-14: trust 8595's public tag names a
    /// staging commit that no longer exists). An overlay lets one target pin its own
    /// build without touching another's, and an old client that does not know this key
    /// ignores it (the index parser tolerates unknown keys), so it is additive on the
    /// wire. Absent ⇒ no overlay: `for_target` is the identity.
    #[serde(default)]
    pub pin_by_target: BTreeMap<String, BTreeMap<String, u64>>,
    // A `[channels.meta]` reproducibility tuple was designed (TOOLCHAIN-PACKAGE-MANAGER.md
    // §4.1) and never published or read; the field that stored it went on 2026-09-27. An
    // index that carries the table still parses: unknown keys are ignored.
}

impl Channel {
    /// The effective yank floor for ONE program: the MAX of the channel-wide
    /// [`Self::min_build`] and that program's [`Self::min_build_by_program`] entry.
    ///
    /// EVERY floor comparison in the client goes through here — [`crate::gate::decide`],
    /// [`crate::gate::current_build_ok`] and the rollback target predicate
    /// ([`crate::flow::rollback`]) — because a build number is only comparable to another build number OF THE SAME PROGRAM. The
    /// counters in one channel are independent (`nn = 108` and `trust-mc = 20065` are both
    /// current), so comparing one program's pin against a floor meant for another is a
    /// category error: it would tombstone every working tool whose numbering happens to be
    /// smaller. A channel that sets no per-program entry keeps the channel-wide floor for
    /// every program, exactly as before the key existed.
    #[must_use]
    pub fn min_build_for(&self, program: &str) -> u64 {
        self.min_build
            .max(self.min_build_by_program.get(program).copied().unwrap_or(0))
    }

    /// This channel as seen from ONE target: [`Self::pin`] with `pin_by_target[triple]`
    /// laid over it — an overlay entry REPLACES the platform-agnostic pin for that program
    /// and an entry for a program `pin` never named ADDS it. Every other field is carried
    /// unchanged, and `pin_by_target` itself is cleared in the view so a consumer cannot
    /// overlay twice. Programs pinned for OTHER targets are not this target's business:
    /// they neither appear nor mask anything.
    ///
    /// Every consumer that plans, decides or fetches for a host goes through this view,
    /// so `plan_groups`, `decide` and `verified_pkg` stay pure over `pin` and never learn
    /// that targets exist — the overlay is resolved once, where the channel is chosen.
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
    /// The named channel as seen from `triple` — [`Channel::for_target`] over the channel
    /// of that name — or `None` when the index carries no such channel. THE one lookup
    /// every host-facing path uses, so a per-target pin can never be missed by a caller
    /// that read `channels` directly.
    #[must_use]
    pub fn channel_for(&self, name: &str, triple: &str) -> Option<Channel> {
        self.channels
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.for_target(triple))
    }

    /// The named program, **iff** the verified index names it. `None` ⇒ the repo is
    /// unreachable (R4): private-config repos, half-finished repos, anything unlisted is
    /// never named, so this is exclusion *by construction*, not by heuristic.
    #[must_use]
    pub fn program(&self, name: &str) -> Option<&Program> {
        self.programs.get(name)
    }

    /// Whether `name` is an installable program named by the index. The fail-closed
    /// reachability rule (§5): an unlisted name is not installable, full stop.
    #[must_use]
    pub fn is_program(&self, name: &str) -> bool {
        self.programs.contains_key(name)
    }

    /// The installable program set: every program the index names, less the
    /// **narrowing-only** `[packages].exclude` (R4/§5). The signed index is the sole gate,
    /// so no config can make a private-config / unlisted repo installable.
    #[must_use]
    pub fn installable(&self, exclude: &[String]) -> BTreeSet<String> {
        let mut set: BTreeSet<String> = self.programs.keys().cloned().collect();
        for e in exclude {
            set.remove(e);
        }
        set
    }
}

/// A release-key-signed `pkg-<program>-<build>.toml` (§4.2): the per-triple artifact
/// matrix for one program build.
#[derive(Debug, Clone, Deserialize)]
pub struct PkgManifest {
    /// Manifest format version; `> SUPPORTED_SCHEMA` is refused.
    pub schema: u32,
    /// The program name — must equal the index `[programs]` key that pointed here; bound
    /// inside the signed bytes so a valid signature can't be paired with a re-pointed
    /// program (the caller cross-checks it against the requested program).
    pub program: String,
    /// Human version string (informational / display).
    #[serde(default)]
    pub version: String,
    /// Monotonic build number — the strictly-greater downgrade gate (reused from the
    /// updater's `build_number` semantics).
    pub build_number: u64,
    /// The binaries to shim into `bin/` (§10) — generic over multi-binary / oddly-named
    /// programs (R2).
    #[serde(default)]
    pub exposes: Vec<String>,
    /// Runtime dependencies — other index-named programs this build needs at runtime.
    /// Resolved at install ([`crate::flow::install`]): each MISSING dep is installed FIRST;
    /// a yanked/below-floor, unreachable, not-pinned, or cyclic dep is SKIPPED with a
    /// warning. A `requires` edge can pull a program IN — it can NEVER bypass the floor/yank
    /// gate ([`crate::gate::decide`]) or the §5 index reachability rule. SIGNED metadata:
    /// parsed only from a [`VerifiedBytes`], so a repo-write adversary cannot inject a
    /// dependency edge.
    #[serde(default)]
    pub requires: Vec<String>,
    /// `shim_env = ["NAME=VALUE", …]` — the environment EVERY shim of this program
    /// (primary and `alab-` alias) exports before it execs the store binary (design S7,
    /// `crates/atpkg/src/shim_env.rs`): a managed vendor tool runs with its own updater
    /// off (`DISABLE_AUTOUPDATER=1` for Claude Code) and the index re-pin is its update
    /// path. Only the MANAGED copy is affected — a system copy never runs through the
    /// shim. SIGNED metadata, validated at parse ([`crate::shim_env::ShimEnv::admit`],
    /// [`Reject::ShimEnv`]); absent on every manifest published before the key existed,
    /// whose shims are byte-identical to before. Read through [`PkgManifest::shim_env`].
    #[serde(default)]
    pub shim_env: Vec<String>,
    /// `[[artifact]]` — one row per target triple. "No row for my triple" is a clean
    /// fail-closed skip, never an error (§6).
    #[serde(default, rename = "artifact")]
    pub artifacts: Vec<Artifact>,
}

/// The six target triples a program may carry rows for (§17.1) — the spelling a row's
/// `target` uses and the one the client reports for itself (`cli::current_triple`, pinned
/// to this list by a test). A program with NO row for the running triple is the
/// canonical `unavailable on <target>: <hint>` state, never an error.
pub const TARGETS: &[&str] = &[
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
];

/// The tools the published trust bundle shims (the pkg-trust-6808 set): the producer packs
/// and refuses a seal without each, and the candidate gate measures the directory that
/// holds them. `ty` left it 2026-08-18 when the standalone `ty` pack took the name: never
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

/// One `[[artifact]]` row: how ONE target triple obtains and applies this build. Two
/// axes (§17): `kind` is the payload / apply shape, `protocol` is where the bytes come
/// from; [`crate::dispatch::strategy_for`] maps the pair to an apply strategy and refuses
/// every pair it does not know. A program carries one row per target it serves
/// ([`TARGETS`]); a target with NO row is the canonical `unavailable on <target>` state,
/// never an error.
#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    /// Target triple (`aarch64-apple-darwin`, `x86_64-apple-darwin`,
    /// `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`,
    /// `aarch64-pc-windows-msvc`).
    pub target: String,
    /// The payload / apply shape: `binary` | `cargo-src` | `sysroot-bundle` | `app-bundle`
    /// (§4.2/§17). The former `vendor-fetch` spelling — never published —
    /// is RETIRED and refused at parse ([`crate::sig::Reject::RetiredKind`]): it was a
    /// protocol wearing a kind's name; write `kind = "binary"` (or `"app-bundle"` +
    /// `payload = "dmg"`) with `protocol = "https"` instead.
    #[serde(default)]
    pub kind: String,
    /// How the bytes are obtained: `github-release` (default — a release asset under the
    /// account slug) | `https` (a vendor's own download, pinned by this signed row).
    /// Absent ⇒ `github-release`, so every manifest published before this key reads
    /// exactly as it did. The OS-installer protocols (`pkg`, `system-pm`,
    /// `softwareupdate`) were deleted 2026-09-24: a row naming one still parses and is
    /// refused at admission ([`crate::vendor::check_row`]) and dispatch alike.
    #[serde(default = "default_protocol")]
    pub protocol: String,
    /// `github-release`: the release asset file name to download. `https`: the LOCAL
    /// staging file name the download lands under (one bare component, never a path).
    /// Validated non-empty for both protocols at parse ([`parse_pkg`]).
    #[serde(default)]
    pub asset: String,
    /// SHA-256 of the DOWNLOADED bytes — the download-integrity gate. Required on every
    /// index row (validated at parse).
    #[serde(default)]
    pub sha256: String,
    /// SHA-512 of the DOWNLOADED bytes, for a vendor-direct row whose vendor signs only
    /// that; empty is absent. A row's gate is the ONE of `sha256`/`sha512` it sets
    /// ([`Artifact::digest`]), and an index row sets `sha256` alone.
    #[serde(default)]
    pub sha512: String,
    /// SHA-256 over the sorted extracted-file list — the apply-time re-verify root (§8
    /// TOCTOU). Empty until producers emit it.
    #[serde(default)]
    pub tree_root: String,
    /// Asset size in bytes — drives the per-artifact download cap + disk preflight.
    #[serde(default)]
    pub size: u64,
    /// Relocation policy for a `sysroot-bundle` (§10.1) — decides the install-time
    /// apply branch. `self-contained` (default): the payload was relocated at PACK
    /// time (machine-local deps vendored in), so install just extracts + activates,
    /// needing NO rustup on the user side. `rustup-linked` (retired: the installer
    /// no longer re-points such a bundle's dangling `toolchain` link, and no
    /// published manifest selects it). Signed (inside the manifest
    /// bytes), so the flag cannot be flipped by a repo-write adversary. Ignored for
    /// non-bundle kinds.
    #[serde(default = "default_reloc")]
    pub reloc: String,
    /// `[artifact.cost]` — honest accounting surfaced before any byte moves (R7).
    #[serde(default)]
    pub cost: Cost,
    /// `https`: the vendor's per-version HTTPS URL the client downloads from.
    /// SIGNED (inside the manifest bytes) and further narrowed by the compiled host
    /// allow-list ([`crate::vendor::VENDOR_HOSTS`]); the host is a transport, never an
    /// authenticity input — `sha256`/`tree_root` gate the bytes exactly as for a release
    /// asset. Ignored for every other protocol.
    #[serde(default)]
    pub url: String,
    /// `https` only: how the download is staged — `raw-binary` | `tar-gz-binary` |
    /// `tar-gz` | `tar-zst` | `zip` (each with `kind = "binary"`) | `dmg` (with
    /// `kind = "app-bundle"`) ([`crate::vendor::PAYLOADS`]).
    #[serde(default)]
    pub payload: String,
    /// `raw-binary` / `tar-gz-binary` only: the LOGICAL tool name the download (or the
    /// tar's one file) becomes under `bin/` (mode `0755`) — laid down under the
    /// platform's executable spelling ([`crate::store::ToolName::exe_file`]: `bin/claude`
    /// on Unix, `bin/claude.exe` on Windows, which is what the shim forwards to). Must be
    /// one of the manifest's `exposes`.
    #[serde(default)]
    pub entry: String,
    /// Archive payloads only (`tar-gz`/`tar-zst`/`zip`): leading path components to drop
    /// on extraction (`gh_<ver>_macOS_arm64/bin/gh` → `bin/gh`). Default `0`.
    #[serde(default)]
    pub strip_components: u32,
    /// `dmg` (and, if declared, archive) payloads: RELATIVE symlinks created at
    /// `bin/<name>` → `../<target>` after staging, so the shims resolve `bin/<tool>`
    /// (`emacs = "Emacs.app/Contents/MacOS/Emacs"`). Every key must be in `exposes`;
    /// targets are relative, `..`-free paths inside the staged tree.
    #[serde(default)]
    pub links: BTreeMap<String, String>,
    /// Display-only vendor name for the consent copy (`"Anthropic PBC"`). NEVER a trust
    /// input.
    #[serde(default)]
    pub vendor: String,
    /// `[artifact.provenance]` — which compiler built THIS row's payload, as the packer's
    /// lineage scan recorded it ([`crate::lineage`]; `atpkg-publish`'s manifest writer
    /// writes it from the scan's record and refuses to sign a `github-release` row without
    /// one). Per ROW, not per manifest: a merge appends the rows of other triples,
    /// packed on other builders with other seals, to one manifest. Signed metadata for
    /// people and audits; no install decision reads it. Absent on every row published
    /// before the table existed, and ignored by every client older than it (no struct
    /// here denies unknown fields).
    #[serde(default)]
    pub provenance: Option<Provenance>,
}

impl Artifact {
    /// The signed digest that gates this row's download: whichever ONE of `sha256` and
    /// `sha512` it sets ([`one_digest`]). `None` — no digest, which no file matches — when
    /// it sets neither or both.
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

/// The ONE digest a `sha256`/`sha512` pair of keys names — a signed row's, a log entry's,
/// a record's: `(algorithm, value)` when exactly one is non-empty, `None` when neither or
/// both are. The value is as written; its shape is the caller's check.
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
    /// How its lineage was proven: `lineage`, `seed-mint`, or `unproven` (admitted while
    /// the receipt requirement was dormant).
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
    /// Evidence of another compiler it found (always `0` in a signed row). `0` means
    /// no evidence was found, not that none could exist (`lineage`'s module docs).
    #[serde(default)]
    pub scan_findings: String,
}

/// A `sysroot-bundle` with no explicit policy is treated as `self-contained` —
/// the safe default (extract-and-run; no assumption the user has rustup).
fn default_reloc() -> String {
    "self-contained".to_string()
}

/// A row with no `protocol` is a release asset under the account slug — every manifest
/// published before the key existed.
fn default_protocol() -> String {
    "github-release".to_string()
}

/// The retired `kind` spelling, and the split that replaced it — the message
/// [`Reject::RetiredKind`] carries.
const VENDOR_FETCH_RETIRED: &str = "kind = \"vendor-fetch\" is retired (it was never \
    published): a row now declares its payload SHAPE as kind = \"binary\" (or \
    \"app-bundle\" with payload = \"dmg\") and where the bytes come from as \
    protocol = \"https\"";

/// `[artifact.cost]` — the honest, structural accounting block (§4.2/R7).
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
    /// The ADMITTED shim environment ([`crate::shim_env::ShimEnv`]). Total: [`parse_pkg`]
    /// already refused a manifest whose list breaks the rule, so this cannot fail on a
    /// parsed manifest; a hand-built one that would is read as no environment (fail-closed
    /// — a shim laid with no env, never a half-parsed one).
    #[must_use]
    pub fn shim_env(&self) -> crate::shim_env::ShimEnv {
        crate::shim_env::ShimEnv::admit(&self.shim_env).unwrap_or_default()
    }

    /// The artifact for `target`, if this build ships one for that triple. `None` is the
    /// clean fail-closed skip (§6) — not an error.
    #[must_use]
    pub fn artifact_for(&self, target: &str) -> Option<&Artifact> {
        self.artifacts.iter().find(|a| a.target == target)
    }

    /// Whether this manifest's signed `program` field matches the program the client
    /// asked for. The anti-replay bind (§4.2): because `program` is inside the signed
    /// bytes, a valid signature can never be paired with a re-pointed program — but the
    /// caller must still CHECK it, so a `pkg.toml` legitimately signed for program A is
    /// refused when fetched as program B. Fail closed at the call site on `false`.
    #[must_use]
    pub fn is_for(&self, name: &str) -> bool {
        self.program == name
    }
}

/// Parse a root-verified `index.toml` from its [`VerifiedBytes`]. Strict UTF-8 (no lossy
/// substitution — the signature was checked over these exact bytes), real `toml` parse,
/// then the [`SUPPORTED_SCHEMA`] reject-newer gate. Any failure is a fail-closed
/// [`Reject`]; the caller treats every variant as "refuse, install nothing".
///
/// `pub(crate)`, deliberately, and narrowing it was a fix rather than tidiness: an
/// `Index` carries self-declared attribution (`machine_id`/`roster_seq`) that is only
/// trustworthy after `TrustedRoster::authorize_index` runs the id↔key bind over it. A
/// public parse entry would let an out-of-crate caller pair `authorize_bytes` with this
/// and read an UNBOUND `machine_id` — signed by one fleet machine, labelled as another.
/// Keeping the parse crate-private makes `authorize_index` the only way to obtain a
/// parsed `Index` from outside, so the bind cannot be skipped by construction.
pub(crate) fn parse_index(verified: &VerifiedBytes) -> Result<Index, Reject> {
    let idx: Index = parse_toml(verified)?;
    if idx.schema > SUPPORTED_SCHEMA {
        return Err(Reject::Schema);
    }
    Ok(idx)
}

/// Parse a release-verified `pkg-*.toml` from its [`VerifiedBytes`] (same strict UTF-8 +
/// reject-newer discipline as [`parse_index`]).
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
/// checked ONCE at parse so no writer ever meets an entry it cannot embed. A refusal is
/// [`Reject::ShimEnv`] carrying the entry and the reason — post-verify, never a crypto
/// oracle — and it refuses the WHOLE manifest: a program whose declared environment
/// cannot be laid must not be installed with a different one.
fn validate_shim_env(m: &PkgManifest) -> Result<(), Reject> {
    crate::shim_env::ShimEnv::admit(&m.shim_env)
        .map(|_| ())
        .map_err(Reject::ShimEnv)
}

/// The post-parse shape rules `serde` alone cannot express, since `asset`/`sha256` are
/// `serde`-optional (a row of a protocol this client does not know may omit them, and is
/// refused by name at admission instead):
///
/// * the retired `kind = "vendor-fetch"` is refused with the split's spelling
///   ([`Reject::RetiredKind`]) — a whole-manifest refusal, since a publisher that emits
///   it is running tooling this schema left behind;
/// * a `github-release` or `https` row still REQUIRES `asset` and `sha256` (they were
///   `serde`-required fields before the split, and a release-shaped row without them
///   used to fail closed as [`Reject::Malformed`] — it still does), and carries no
///   `sha512`: an index row's gate is its `sha256`, never a second digest beside it.
///
/// Everything finer — hosts, payload lanes, an unknown protocol — is
/// [`crate::vendor::check_row`]'s per-program refusal, so one mis-authored row for one
/// target never takes the other targets' rows down with it.
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

/// Shared strict-UTF-8 + `toml` deserialize over already-verified bytes. Invalid UTF-8
/// or any TOML/shape error (missing required field, duplicate key, wrong type) is
/// [`Reject::Malformed`] — fail closed, never a lossy reinterpretation.
fn parse_toml<T: serde::de::DeserializeOwned>(verified: &VerifiedBytes) -> Result<T, Reject> {
    let text = std::str::from_utf8(verified.as_slice()).map_err(|_| Reject::Malformed)?;
    aterm_toml::from_str(text).map_err(|_| Reject::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sig::testkit;

    /// `body`, machine-signed and taken through the REAL roster chain, so `parse_index` /
    /// `parse_pkg` run on genuinely-verified input. (Parsing raw bytes does not
    /// type-check, which is the compile-time half of the guarantee and needs no test.)
    fn verified(body: &str) -> VerifiedBytes {
        testkit::machine_signed(body.as_bytes().to_vec())
    }

    /// A complete, realistic index naming three programs (and deliberately NOT naming a
    /// private-config repo like `dotfiles`), attributed to the test roster's machine.
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

    /// REGRESSION (audit 2026-09-15): the yank floor a multi-program channel needs is PER
    /// PROGRAM. A channel-wide floor is compared against every program's own counter, and the
    /// counters are independent (`nn = 108` beside `trust-mc = 20065` on the live index), so
    /// the only channel-wide value that did not tombstone unrelated programs was 0.
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
        // And an index WITHOUT the key is unchanged: every program keeps the channel-wide
        // floor, so an old published index decides exactly as it did before the key existed.
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
        // The attribution pair that replaced `[keys]` — what the roster bind checks.
        assert_eq!(idx.machine_id.as_deref(), Some(testkit::MACHINE_ID));
        assert_eq!(idx.roster_seq, Some(testkit::SEQ));
        // Programs, channels and pin parsed; the fixture's `[channels.meta]` table, which
        // no field reads, is tolerated as an unknown key (wire-safe for any index that
        // ever carried one).
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

    /// A per-target overlay changes ONLY the target it names: that target sees the
    /// overlaid build for a program `pin` also names (replace), sees a program `pin`
    /// never named (add), and every other target sees `pin` exactly as published.
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

    /// The set a publisher must carry: over every triple an index names, the builds clients
    /// resolve are `pin` union every `pin_by_target` overlay, never one of the two. This is the
    /// law the public mirror's work list (`atpkg-publish mirror`) derives from; reading `pin` alone leaves a toolchain that publishes
    /// entirely as an overlay pinned but never mirrored. Both halves below make it necessary:
    ///
    ///   * an overlay build is resolvable and appears in no `pin` row (so `pin` alone is
    ///     not enough), and
    ///   * the `pin` build an overlay masks is still resolvable from every other triple
    ///     (so the overlays alone are not enough, and a masked build may never be dropped).
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

        // …and neither key alone covers it, which is the whole reason for a union.
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
        // Sanity: the named programs ARE reachable.
        assert!(idx.is_program("trust") && idx.is_program("ay") && idx.is_program("aterm"));
    }

    // §5: exclude is NARROWING-ONLY.
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
        // Absent keys default: an ordinary binary row reads back empty/zero, and the
        // protocol it never declared is the release lane every older manifest meant.
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

    /// `shim_env` (design S7) parses as SIGNED data and is validated at parse: the
    /// accepted shape reads back through `PkgManifest::shim_env`, a manifest without the
    /// key reads as no environment, and every entry the rule refuses refuses the WHOLE
    /// manifest as `Reject::ShimEnv` naming the entry — never `Malformed`, never a shim
    /// laid with half an environment.
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

    /// The retired index-level `requires` (§5.3(b)) still PARSES — whatever it names, a
    /// cycle and an unlisted name included, since nothing orders on it any more — so the
    /// program that names it can be refused by name rather than the whole index.
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
        // Even a SECOND row spelled that way refuses the manifest: a publisher emitting it
        // is on retired tooling.
        let mixed = "schema = 2\nprogram = \"claude\"\nbuild_number = 1\n[[artifact]]\n\
                     target = \"aarch64-apple-darwin\"\nasset = \"a\"\nsha256 = \"d\"\n\
                     [[artifact]]\ntarget = \"x86_64-apple-darwin\"\nkind = \"vendor-fetch\"\n\
                     asset = \"a\"\nsha256 = \"d\"\n";
        assert!(matches!(
            parse_pkg(&verified(mixed)),
            Err(Reject::RetiredKind(_))
        ));
    }

    /// A row of a DELETED protocol (`pkg`, `system-pm`, §5.3(b)) still parses, keys and
    /// all, so admission refuses that one program by name rather than the manifest being
    /// malformed — while a release-shaped row still fails closed without `asset`/`sha256`,
    /// exactly as when serde required them.
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
        // Release-shaped rows keep their required digests (the pre-split serde contract),
        // and an index row's digest is its sha256 alone.
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

    /// A row's gate is the ONE digest it sets, named by its algorithm; neither or both is
    /// no digest. The shape check takes each algorithm's length, lowercase only — the
    /// sample is agy 1.3.6's darwin_arm64 tarball sha512 as Google's manifest names it.
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

    /// Every published triple is a forge cell, so some lane's compiler reads it:
    /// a shipped triple in no cell is compiled by none.
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
        // And the CURRENT schema is accepted — so the gate is the number, not the fixture.
        assert!(parse_index(&verified(&full_index())).is_ok());
    }

    /// A schema-1 index — the shape the retired delegation tier published — still PARSES
    /// (1 ≤ SUPPORTED_SCHEMA) but carries no attribution, so the roster bind refuses it.
    /// Both halves matter: the parse proves this is not an accidental format break, and
    /// the missing attribution proves an old-shape index installs nothing.
    #[test]
    fn a_schema_one_index_parses_but_carries_no_attribution() {
        let body = "schema = 1\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
                    [keys]\nrelease_key_id = \"rk\"\nrelease_key_pubkey = \"AAAA\"\n";
        let idx = parse_index(&verified(body)).expect("an older schema is not a parse failure");
        assert_eq!(idx.machine_id, None);
        assert_eq!(idx.roster_seq, None);
        // The bind is what refuses it; `sig`'s tests prove that direction end to end.
    }

    /// Each of these fails closed as `Malformed`, never a silent default. One labelled row
    /// per shape:
    /// * a missing required field (`index_build`) is not a default-0 silent accept;
    /// * a DUPLICATE key — the real TOML parser gives this for free (the Phase-1 line
    ///   scanner had to hand-defend it) — matters for `machine_id`, where a last-wins
    ///   parser would let a second copy re-attribute the index;
    /// * a wrong-typed attribution: a string `roster_seq` cannot become "absent" and
    ///   slide into the bind.
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

    // Table scoping is intrinsic to the real parser: a `machine_id` in a SIBLING table
    // cannot shadow the genuine top-level attribution (it lands in an ignored table).
    #[test]
    fn sibling_table_cannot_hijack_the_attribution() {
        let body = "schema = 2\nindex_build = 1\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
                    machine_id = \"m3\"\nroster_seq = 3\n\
                    [meta]\nmachine_id = \"m11\"\nroster_seq = 9\n";
        let idx = parse_index(&verified(body)).expect("parses; [meta] ignored");
        assert_eq!(idx.machine_id.as_deref(), Some("m3"));
        assert_eq!(idx.roster_seq, Some(3));
    }

    // pkg-*.toml: per-triple artifact selection + reject-newer + clean missing-triple skip.
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

    // STRICT, never-lossy UTF-8: a signed index containing an invalid-UTF-8 byte is
    // rejected (Malformed), NOT silently U+FFFD-substituted and reinterpreted. The byte
    // is part of the genuinely-verified bytes, so this exercises the parse-layer's
    // from_utf8 arm (not the signature).
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

    // parse_pkg negative paths (symmetry with the index gate): a newer schema is refused,
    // and a missing required field fails closed.
    #[test]
    fn parse_pkg_rejects_newer_schema_and_missing_field() {
        let newer = "schema = 99\nprogram = \"ay\"\nbuild_number = 1\n";
        assert_eq!(parse_pkg(&verified(newer)).unwrap_err(), Reject::Schema);
        // Missing build_number (required) → Malformed, not a default-0 silent accept.
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

    // The signed `program` field binds the manifest to a program (§4.2 anti-replay): a
    // pkg legitimately signed for "ay" must be refused when fetched as some other program.
    // This bind is UNCHANGED by the single-root move — it is the pkg tier's id bind, and
    // it is what the roster's `machine_id` bind is for the index.
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
