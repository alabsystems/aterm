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
/// Both directions fail CLOSED, twice over. See `docs/ATPKG-KEY-MANAGEMENT.md` for what
/// an already-installed client does about it (short answer: reinstall, accepted).
pub const SUPPORTED_SCHEMA: u32 = 2;

/// The default repository the signed index lives on, under the configurable account:
/// `github.com/<account>/aterm` (§5). The index is a small signed release asset on the
/// **aterm repo itself** — no dedicated repo to administer, 1-to-1 with the existing
/// repos, and coherent with §16 (aterm is itself an index member). Overridable at runtime
/// via `ATPKG_INDEX_REPO` (see [`crate::discovery::index_repo`]).
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
    /// `extra = true` ⇒ listed and pinned (installable by name: `atpkg install <name>`,
    /// Settings, or the typed-name consent stub) but NOT a default-set member:
    /// [`Index::installable`] omits it unless `include` names it or an opt-in marker
    /// (`<prefix>/optin/<name>`, [`crate::store::Layout::optin_exists`]) records that this
    /// machine asked for it. Absent ⇒ `false` (today's behaviour). A client older than this
    /// key ignores it and treats the row as default-set — the rollout accepts that.
    #[serde(default)]
    pub extra: bool,
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
    /// `requires = ["clt"]` — index programs that must be installed (managed, system-
    /// satisfied, or `installed via <protocol>`) BEFORE this one. Homebrew requires `clt`:
    /// its `.pkg` refuses to install without the Command Line Tools' git. Validated at
    /// index parse ([`validate_requires`]: every name an index program, no self-
    /// dependency, no cycle — over programs or over coherence groups); ordered by the
    /// plan ([`crate::apply::plan_groups`] is dependency-first); gated by the pass (a
    /// member whose requirement is unmet records `blocked by <dep>: <dep state>` and
    /// downloads nothing — [`crate::state::blocked`]); resolved by
    /// [`crate::flow::install`] exactly like a pkg manifest's own `requires`, unioned with
    /// it — the explicit door installs the dependency first, in the same session. For a
    /// member applied through an OS installer (`pkg`, `softwareupdate`) a requirement
    /// that is DEFERRED (`needs admin`) defers the member too, and one that failed stops
    /// it — the installer would only fail later, less legibly. A requirement on an EXTRA
    /// is allowed: the consent surfaces name it and it is never opted in on the
    /// dependent's behalf. Absent ⇒ none.
    #[serde(default)]
    pub requires: Vec<String>,
}

/// One `[[channels]]` entry: a named, pinned set of program builds plus the gating
/// counters and the attested reproducibility tuple (`[channels.meta]`).
#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    /// Channel name (`stable`, `nightly`).
    pub name: String,
    /// Monotonic no-downgrade gate for the channel's coherence group.
    #[serde(default)]
    pub channel_build: u64,
    /// Yank floor: a pinned build below this is force-upgraded / tombstoned at apply (§7).
    #[serde(default)]
    pub min_build: u64,
    /// Per-program revocations (`"trust@4790"`), enforced at apply (§7).
    #[serde(default)]
    pub yanked: Vec<String>,
    /// The pinned SET — exact per-program builds that move together (`program -> build`).
    #[serde(default)]
    pub pin: BTreeMap<String, u64>,
    /// `[channels.meta]` — the attested reproducibility tuple (nightly id, trust-mc rev,
    /// …). Stored generically here; Phase 4/5 validate it. Not all fields are attested
    /// (§4.1 — `trust_fork_rev`/`llvm`/`clean_kernel_rev` are net-new, unproven).
    #[serde(default)]
    pub meta: BTreeMap<String, String>,
}

impl Index {
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

    /// The installable program set after applying the **narrowing-only**
    /// `[packages].include`/`exclude` config (R4/§5). The signed index is the sole gate:
    ///
    /// * empty `include` ⇒ start from *every* DEFAULT-SET program the index names — every
    ///   program without `extra = true` ([`Program::extra`]);
    /// * non-empty `include` ⇒ start from only those of its names that the index **also**
    ///   names (an `include` entry absent from the index adds **nothing** — it can never
    ///   widen the set or introduce an unlisted repo). `include` MAY name an extra: it is
    ///   index-named, so this never widens past the signed set;
    /// * `exclude` then subtracts.
    ///
    /// So no config can make a private-config / unlisted repo installable. The opt-in
    /// markers a machine records for extras are a THIRD input, carried by
    /// [`Index::installable_with_optins`]; this form is that one with no markers.
    #[must_use]
    pub fn installable(&self, include: &[String], exclude: &[String]) -> BTreeSet<String> {
        self.installable_with_optins(include, exclude, &BTreeSet::new())
    }

    /// [`Index::installable`] plus the machine's recorded opt-ins: an extra whose name is
    /// in `optins` joins the set exactly as if `include` had named it. `optins` is still
    /// narrowing-only over the signed index — an opt-in for a name the index does not
    /// carry adds nothing — and `exclude` subtracts after it, so a config exclusion still
    /// beats a marker.
    #[must_use]
    pub fn installable_with_optins(
        &self,
        include: &[String],
        exclude: &[String],
        optins: &BTreeSet<String>,
    ) -> BTreeSet<String> {
        let mut set: BTreeSet<String> = if include.is_empty() {
            self.programs
                .iter()
                .filter(|(_, p)| !p.extra)
                .map(|(n, _)| n.clone())
                .collect()
        } else {
            include
                .iter()
                .filter(|n| self.programs.contains_key(n.as_str()))
                .cloned()
                .collect()
        };
        for name in optins {
            if self.programs.get(name.as_str()).is_some_and(|p| p.extra) {
                set.insert(name.clone());
            }
        }
        for e in exclude {
            set.remove(e);
        }
        set
    }

    /// Whether `name` is an index-named EXTRA ([`Program::extra`]): available on request,
    /// never a default-set member. `false` for a default-set program AND for a name the
    /// index does not carry (nothing outside the signed set is an extra either).
    #[must_use]
    pub fn is_extra(&self, name: &str) -> bool {
        self.programs.get(name).is_some_and(|p| p.extra)
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
    /// | `installer-pkg` (the `pkg` protocol only) | `system-package` (the `system-pm` and
    /// `softwareupdate` protocols) (§4.2/§17). The former `vendor-fetch` spelling — never published —
    /// is RETIRED and refused at parse ([`crate::sig::Reject::RetiredKind`]): it was a
    /// protocol wearing a kind's name; write `kind = "binary"` (or `"app-bundle"` +
    /// `payload = "dmg"`) with `protocol = "https"` instead.
    #[serde(default)]
    pub kind: String,
    /// How the bytes are obtained: `github-release` (default — a release asset under the
    /// account slug) | `https` (a vendor's own download, pinned by this signed row) |
    /// `pkg` (a Developer-ID-signed macOS installer package, applied with elevation) |
    /// `system-pm` (the platform's own package manager) | `softwareupdate` (Apple's
    /// `softwareupdate`, macOS only — the Command Line Tools). Absent ⇒ `github-release`,
    /// so every manifest published before this key reads exactly as it did.
    #[serde(default = "default_protocol")]
    pub protocol: String,
    /// `github-release`: the release asset file name to download. `https`/`pkg`: the
    /// LOCAL staging file name the download lands under (one bare component, never a
    /// path). `system-pm`: unused. Validated non-empty for the two release-shaped
    /// protocols at parse ([`parse_pkg`]).
    #[serde(default)]
    pub asset: String,
    /// SHA-256 of the DOWNLOADED bytes — the download-integrity gate. Required (validated
    /// at parse) for every protocol that moves bytes; `system-pm` carries none.
    #[serde(default)]
    pub sha256: String,
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
    /// needing NO rustup on the user side. `rustup-linked`: the bundle ships a
    /// dangling `toolchain` link the installer re-points at the user's rustup
    /// nightly ([`crate::sysroot::relocate_sysroot`]). Signed (inside the manifest
    /// bytes), so the flag cannot be flipped by a repo-write adversary. Ignored for
    /// non-bundle kinds.
    #[serde(default = "default_reloc")]
    pub reloc: String,
    /// `[artifact.cost]` — honest accounting surfaced before any byte moves (R7).
    #[serde(default)]
    pub cost: Cost,
    /// `https` / `pkg`: the vendor's per-version HTTPS URL the client downloads from.
    /// SIGNED (inside the manifest bytes) and further narrowed by the compiled host
    /// allow-list ([`crate::vendor::VENDOR_HOSTS`]); the host is a transport, never an
    /// authenticity input — `sha256`/`tree_root` gate the bytes exactly as for a release
    /// asset. Ignored for every other protocol.
    #[serde(default)]
    pub url: String,
    /// `https` only: how the download is staged — `raw-binary` | `tar-gz` | `tar-zst` |
    /// `zip` (each with `kind = "binary"`) | `dmg` (with `kind = "app-bundle"`)
    /// ([`crate::vendor::PAYLOADS`]).
    #[serde(default)]
    pub payload: String,
    /// `raw-binary` only: the LOGICAL tool name the download becomes under `bin/` (mode
    /// `0755`) — laid down under the platform's executable spelling
    /// ([`crate::store::ToolName::exe_file`]: `bin/claude` on Unix, `bin/claude.exe` on
    /// Windows, which is what the shim forwards to). Must be one of the manifest's
    /// `exposes`.
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
    /// `pkg` only: the Apple Developer ID TEAM of the `.pkg`'s signer (`"927JGANW46"`),
    /// which the installer lane checks against `pkgutil --check-signature` before the
    /// package is applied. Required for the protocol ([`crate::vendor::check_row`]).
    #[serde(default)]
    pub signer_team: String,
    /// Whether applying this row needs elevation (an administrator prompt). Required
    /// `true` for `pkg`; required `true` for `system-pm` over `apt`/`dnf`. The unattended
    /// pass never elevates — such a member records the canonical `needs admin — run:
    /// aterm pkg install <name>` state and waits for the explicit door.
    #[serde(default)]
    pub elevated: bool,
    /// `pkg` / `system-pm` / `softwareupdate`: what proves the install happened, since
    /// nothing lands in the store. `pkg` and `softwareupdate`: absolute paths
    /// (`"/opt/homebrew/bin/brew"`, `"/Library/Developer/CommandLineTools/usr/bin/git"`).
    /// `system-pm`: bare tool names resolved on `PATH` (through `PATHEXT` on Windows),
    /// or absolute paths. The first that exists is the `installed via <protocol>: <path>`
    /// state's path — for `system-pm` the MANAGER's name stands in the protocol slot
    /// (`installed via apt: /usr/bin/emacs`).
    #[serde(default)]
    pub provides: Vec<String>,
    /// `system-pm` only: which manager resolves `package` — one row of the extensible
    /// manager table ([`crate::vendor::MANAGER_TABLE`]: `apt` | `dnf` | `brew` | `winget`
    /// | `scoop` | `cargo` | `pipx`). A machine without that manager on `PATH` reads the
    /// member as `unavailable on <target>` — atpkg never installs a manager
    /// ([`crate::system_pm`]).
    #[serde(default)]
    pub manager: String,
    /// `system-pm` only: the package id in that manager's own naming (`emacs`,
    /// `GNU.Emacs`, `GitHub.cli`). Passed to the manager as ONE argument, never a shell
    /// word; never begins with `-`.
    #[serde(default)]
    pub package: String,
    /// `softwareupdate` only: the head of the `softwareupdate -l` label to install
    /// (`"Command Line Tools for Xcode"`); the NEWEST label starting with it is picked
    /// ([`crate::softwareupdate::pick_label`]). Required for the protocol.
    #[serde(default)]
    pub label_prefix: String,
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
    validate_requires(&idx)?;
    Ok(idx)
}

/// The `[programs.<name>].requires` relation must be one a client can honour, checked
/// ONCE at index parse so no plan is ever asked to order what cannot be ordered
/// ([`Reject::Requires`], naming the offending edge):
///
/// * every name is an index program — a `requires` edge can pull a program IN, never
///   reach outside the signed set (§5 stays by construction);
/// * no program requires itself;
/// * no cycle over programs (`a → b → a`), and none THROUGH a coherence group: the plan
///   installs a group as one atomic unit ([`crate::apply::plan_groups`]), so a path that
///   leaves a group and comes back to another of its members (`trust → ny → ay`, with
///   trust and ay both in `rustc`) would wait on itself forever. A dependency BETWEEN two
///   members of one group is fine (the tuple's own transaction satisfies it).
///
/// A dependency on an EXTRA is allowed: the consent surfaces name it, no pass opts in to
/// it on the dependent's behalf, and the dependent reads `blocked by <extra>: extra — not
/// installed (…)` until the user does ([`crate::state::blocked`]).
pub(crate) fn validate_requires(idx: &Index) -> Result<(), Reject> {
    for (name, p) in &idx.programs {
        for dep in &p.requires {
            if dep == name {
                let mut m = name.clone();
                m.push_str(" requires itself");
                return Err(Reject::Requires(m));
            }
            if !idx.programs.contains_key(dep) {
                let mut m = name.clone();
                m.push_str(" requires ");
                m.push_str(dep);
                m.push_str(", which the index does not name");
                return Err(Reject::Requires(m));
            }
        }
    }
    let mut colour: BTreeMap<&str, u8> = BTreeMap::new();
    for name in idx.programs.keys() {
        if let Some(cycle) = program_cycle(idx, name, &mut colour, &mut Vec::new()) {
            let mut m = String::from("requires cycle: ");
            m.push_str(&cycle.join(" → "));
            return Err(Reject::Requires(m));
        }
    }
    let mut groups: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (name, p) in &idx.programs {
        if let Some(g) = &p.coherence_group {
            groups.entry(g.as_str()).or_default().insert(name.as_str());
        }
    }
    for (g, members) in &groups {
        for m in members {
            let mut seen = BTreeSet::new();
            if let Some(path) = group_cycle(idx, members, m, &mut Vec::new(), &mut seen, false) {
                let mut msg = String::from("requires cycle through coherence group '");
                msg.push_str(g);
                msg.push_str("': ");
                msg.push_str(&path.join(" → "));
                msg.push_str(" (the group installs as one unit, so it would wait on itself)");
                return Err(Reject::Requires(msg));
            }
        }
    }
    Ok(())
}

/// Depth-first over `requires` edges from `name`; `Some(cycle)` names the first cycle met
/// (`a → b → a`), else `None`. `colour`: absent = unvisited, 1 = on the stack, 2 = done.
fn program_cycle<'a>(
    idx: &'a Index,
    name: &'a str,
    colour: &mut BTreeMap<&'a str, u8>,
    stack: &mut Vec<&'a str>,
) -> Option<Vec<String>> {
    match colour.get(name) {
        Some(2) => return None,
        Some(_) => {
            let start = stack.iter().position(|s| *s == name).unwrap_or(0);
            let mut cycle: Vec<String> = stack[start..].iter().map(|s| (*s).to_string()).collect();
            cycle.push(name.to_string());
            return Some(cycle);
        }
        None => {}
    }
    colour.insert(name, 1);
    stack.push(name);
    if let Some(p) = idx.programs.get(name) {
        for dep in &p.requires {
            if let Some(cycle) = program_cycle(idx, dep.as_str(), colour, stack) {
                return Some(cycle);
            }
        }
    }
    stack.pop();
    colour.insert(name, 2);
    None
}

/// Depth-first from `node` (a member of the group `members`), over every `requires`
/// edge; `left` records whether the path has stepped OUTSIDE the group. Arriving back at
/// a member after having left is a cycle through the group: `Some(path)` names it.
/// `seen` holds `(node, left)` pairs so the walk terminates on any graph.
fn group_cycle<'a>(
    idx: &'a Index,
    members: &BTreeSet<&str>,
    node: &'a str,
    path: &mut Vec<&'a str>,
    seen: &mut BTreeSet<(&'a str, bool)>,
    left: bool,
) -> Option<Vec<String>> {
    if !seen.insert((node, left)) {
        return None;
    }
    path.push(node);
    let mut found = None;
    if let Some(p) = idx.programs.get(node) {
        for dep in &p.requires {
            let inside = members.contains(dep.as_str());
            if inside && left {
                let mut cycle: Vec<String> = path.iter().map(|s| (*s).to_string()).collect();
                cycle.push(dep.clone());
                found = Some(cycle);
                break;
            }
            found = group_cycle(idx, members, dep.as_str(), path, seen, left || !inside);
            if found.is_some() {
                break;
            }
        }
    }
    path.pop();
    found
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

/// The post-parse shape rules `serde` alone cannot express now that `asset`/`sha256` are
/// optional for the protocols that carry no bytes:
///
/// * the retired `kind = "vendor-fetch"` is refused with the split's spelling
///   ([`Reject::RetiredKind`]) — a whole-manifest refusal, since a publisher that emits
///   it is running tooling this schema left behind;
/// * a `github-release` or `https` row still REQUIRES `asset` and `sha256` (they were
///   `serde`-required fields before the split, and a release-shaped row without them
///   used to fail closed as [`Reject::Malformed`] — it still does);
/// * a `pkg` row requires `url` and `sha256` (bytes move; they must be pinned);
/// * `system-pm` and `softwareupdate` rows move no bytes and need nothing here.
///
/// Everything finer — hosts, payload lanes, `signer_team`, `manager`, `provides`,
/// `label_prefix` — is [`crate::vendor::check_row`]'s per-program refusal, so one
/// mis-authored row for one target never takes the other targets' rows down with it.
fn validate_rows(m: &PkgManifest) -> Result<(), Reject> {
    for a in &m.artifacts {
        if a.kind == "vendor-fetch" {
            return Err(Reject::RetiredKind(VENDOR_FETCH_RETIRED));
        }
        let release_shaped = matches!(a.protocol.as_str(), "github-release" | "https");
        if release_shaped && (a.asset.is_empty() || a.sha256.is_empty()) {
            return Err(Reject::Malformed);
        }
        if a.protocol == "pkg" && (a.url.is_empty() || a.sha256.is_empty()) {
            return Err(Reject::Malformed);
        }
    }
    Ok(())
}

/// Shared strict-UTF-8 + `toml` deserialize over already-verified bytes. Invalid UTF-8
/// or any TOML/shape error (missing required field, duplicate key, wrong type) is
/// [`Reject::Malformed`] — fail closed, never a lossy reinterpretation.
fn parse_toml<T: serde::de::DeserializeOwned>(verified: &VerifiedBytes) -> Result<T, Reject> {
    #[cfg(test)]
    PARSE_CALLS.with(|c| c.set(c.get() + 1));
    let text = std::str::from_utf8(verified.as_slice()).map_err(|_| Reject::Malformed)?;
    aterm_toml::from_str(text).map_err(|_| Reject::Malformed)
}

#[cfg(test)]
thread_local! {
    /// Test-only counter proving a parser never runs on unverified bytes: incremented in
    /// [`parse_toml`], asserted to stay flat after a failed verify. Thread-local so
    /// libtest's per-test threads don't race a shared global; `#[cfg(test)]` so it never
    /// ships.
    static PARSE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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

[programs.gh]
repo = "gh"
policy = "prebuilt-only"
system = "gh"

[[channels]]
name = "stable"
channel_build = 137
min_build = 120
yanked = ["trust@4790"]
pin = {{ aterm = 1234, trust = 4821, ay = 18, codex = 2026082601, gh = 2026082601 }}

[channels.meta]
nightly = "nightly-2025-12-03"
trust_mc_rev = "0.67.0"
"#,
            id = testkit::MACHINE_ID,
            seq = testkit::SEQ
        )
    }

    #[test]
    fn parses_a_full_index_and_its_attribution() {
        let idx = parse_index(&verified(&full_index())).expect("valid index parses");
        assert_eq!(idx.index_build, 41);
        assert_eq!(idx.valid_until, "2026-07-05T12:00:00Z");
        // The attribution pair that replaced `[keys]` — what the roster bind checks.
        assert_eq!(idx.machine_id.as_deref(), Some(testkit::MACHINE_ID));
        assert_eq!(idx.roster_seq, Some(testkit::SEQ));
        // Programs, channels, pin, meta all parsed.
        assert_eq!(
            idx.program("trust").unwrap().coherence_group.as_deref(),
            Some("rustc")
        );
        assert_eq!(idx.program("ay").unwrap().repo, "ay");
        assert_eq!(idx.channels.len(), 1);
        let ch = &idx.channels[0];
        assert_eq!(ch.name, "stable");
        assert_eq!(ch.pin.get("trust"), Some(&4821));
        assert_eq!(
            ch.meta.get("nightly").map(String::as_str),
            Some("nightly-2025-12-03")
        );
    }

    /// A LEFTOVER `[keys]` table carries no authority any more: it parses (unknown tables
    /// are ignored, so a producer mid-changeover is not refused for emitting it) and there
    /// is no API that can read a release key out of an index. The delegation tier is gone,
    /// not merely unused — this test would not compile if `Index::delegation` still
    /// existed and something called it.
    #[test]
    fn a_leftover_keys_table_is_ignored_and_grants_nothing() {
        let mut body = full_index();
        body.push_str("\n[keys]\nrelease_key_id = \"rk-2026-06\"\nrelease_key_pubkey = \"AAAA\"\n");
        let idx = parse_index(&verified(&body)).expect("an ignored table is not a refusal");
        assert_eq!(idx.index_build, 41, "the rest of the index still reads");
        // The only authority over a pkg manifest is the roster generation, reached
        // through `TrustedIndex::verify_pkg` — never anything inside these bytes.
        assert_eq!(idx.machine_id.as_deref(), Some(testkit::MACHINE_ID));
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
            !idx.installable(&[], &[]).contains("dotfiles"),
            "a private-config repo can never be in the installable set"
        );
        // Sanity: the named programs ARE reachable.
        assert!(idx.is_program("trust") && idx.is_program("ay") && idx.is_program("aterm"));
    }

    // §5: include/exclude are NARROWING-ONLY — an include naming an absent repo adds nothing.
    #[test]
    fn include_exclude_are_narrowing_only() {
        let idx = parse_index(&verified(&full_index())).unwrap();
        // Default (empty include): every named DEFAULT-SET program (`codex` is an extra
        // and stays out; `gh` is default-set — `system` is a satisfaction rule, not a tier).
        assert_eq!(
            idx.installable(&[], &[]),
            ["aterm", "ay", "gh", "trust"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
        // include narrows to the intersection with named programs...
        assert_eq!(
            idx.installable(&["ay".into(), "trust".into()], &[]),
            ["ay", "trust"].iter().map(|s| s.to_string()).collect()
        );
        // ...and an include naming an ABSENT repo can never add it (no widening).
        assert_eq!(
            idx.installable(&["ay".into(), "dotfiles".into()], &[]),
            ["ay"]
                .iter()
                .map(|s| s.to_string())
                .collect::<BTreeSet<_>>()
        );
        // exclude subtracts.
        assert_eq!(
            idx.installable(&[], &["trust".into(), "gh".into()]),
            ["aterm", "ay"].iter().map(|s| s.to_string()).collect()
        );
    }

    /// The EXTRAS tier: an `extra = true` program is index-named and pinned (reachable by
    /// name) but NOT in the default set. It joins the set when `include` names it or when
    /// this machine recorded an opt-in — and only then.
    #[test]
    fn extras_are_excluded_by_default_and_join_by_include_or_optin() {
        let idx = parse_index(&verified(&full_index())).unwrap();
        // Parsed as declared; absent ⇒ false; `system` parsed beside it.
        assert!(idx.program("codex").unwrap().extra);
        assert!(!idx.program("ay").unwrap().extra);
        assert!(idx.is_extra("codex"));
        assert!(!idx.is_extra("ay"), "a default-set program is not an extra");
        assert!(
            !idx.is_extra("dotfiles"),
            "an unlisted name is not an extra"
        );
        assert_eq!(idx.program("gh").unwrap().system.as_deref(), Some("gh"));
        assert_eq!(idx.program("ay").unwrap().system, None);
        // Reachable by name (the channel pins it) — but not in the default set.
        assert!(idx.is_program("codex"));
        assert!(!idx.installable(&[], &[]).contains("codex"));
        // `include` may name it: index-named, so this never widens past the signed set.
        assert_eq!(
            idx.installable(&["codex".into()], &[]),
            ["codex"].iter().map(|s| s.to_string()).collect()
        );
        // An opt-in marker unions it into the default set...
        let optins: BTreeSet<String> = ["codex".to_string()].into_iter().collect();
        assert_eq!(
            idx.installable_with_optins(&[], &[], &optins),
            ["aterm", "ay", "codex", "gh", "trust"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
        // ...an opt-in for a NON-extra or an unlisted name adds nothing (narrowing-only)...
        let stray: BTreeSet<String> = ["ay".to_string(), "dotfiles".to_string()]
            .into_iter()
            .collect();
        assert_eq!(
            idx.installable_with_optins(&[], &[], &stray),
            idx.installable(&[], &[])
        );
        // ...and `exclude` still beats a marker.
        assert!(
            !idx.installable_with_optins(&[], &["codex".into()], &optins)
                .contains("codex")
        );
        // The no-marker form is the marker form with no markers.
        assert_eq!(
            idx.installable(&[], &[]),
            idx.installable_with_optins(&[], &[], &BTreeSet::new())
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
        assert!(a.signer_team.is_empty() && !a.elevated && a.provides.is_empty());
        assert!(a.manager.is_empty() && a.package.is_empty() && a.label_prefix.is_empty());
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
        assert_eq!(
            m.shim_env().fix_line().as_deref(),
            Some("self-update off (DISABLE_AUTOUPDATER=1)")
        );
        // Absent: no environment, exactly as every manifest published before the key.
        let without = parse_pkg(&verified(&format!("{head}{row}"))).unwrap();
        assert!(without.shim_env.is_empty() && without.shim_env().is_empty());
        // Refused, each by name, as ShimEnv — the publisher's verify-pkg spells the fix.
        for (list, why) in [
            ("[\"DISABLE_AUTOUPDATER\"]", "not NAME=VALUE"),
            ("[\"disable_autoupdater=1\"]", "name is not [A-Z0-9_]+"),
            ("[\"PATH=/tmp\"]", "never sets"),
            ("[\"X=\"]", "empty value"),
            ("[\"X=1\", \"X=2\"]", "duplicate name"),
            (
                "[\"A=1\", \"B=1\", \"C=1\", \"D=1\", \"E=1\", \"F=1\", \"G=1\", \"H=1\", \"I=1\"]",
                "at most 8",
            ),
        ] {
            let body = format!("{head}shim_env = {list}\n{row}");
            match parse_pkg(&verified(&body)) {
                Err(Reject::ShimEnv(m)) => assert!(m.contains(why), "{list}: {m}"),
                other => panic!("{list}: expected Reject::ShimEnv, got {other:?}"),
            }
        }
        // A control byte inside the TOML string is refused the same way (the string
        // escape carries it through the parser; the rule refuses it after).
        let body = format!("{head}shim_env = [\"X=a\\nb\"]\n{row}");
        assert!(matches!(
            parse_pkg(&verified(&body)),
            Err(Reject::ShimEnv(m)) if m.contains("control byte")
        ));
        // The wrong TYPE is the parser's own refusal, as for any key.
        let body = format!("{head}shim_env = \"DISABLE_AUTOUPDATER=1\"\n{row}");
        assert_eq!(parse_pkg(&verified(&body)).unwrap_err(), Reject::Malformed);
    }

    /// A `softwareupdate` row (Apple's Command Line Tools) parses with its own keys and
    /// no bytes at all; the index-level `requires` parses beside `system`/`extra`.
    #[test]
    fn parses_a_softwareupdate_row_and_the_index_requires() {
        let body = r#"
schema = 2
program = "clt"
version = "16.4"
build_number = 2026082701
exposes = []

[[artifact]]
target = "aarch64-apple-darwin"
kind = "system-package"
protocol = "softwareupdate"
label_prefix = "Command Line Tools for Xcode"
elevated = true
provides = ["/Library/Developer/CommandLineTools/usr/bin/git"]
vendor = "Apple"
"#;
        let m = parse_pkg(&verified(body)).expect("a softwareupdate row parses");
        let a = m.artifact_for("aarch64-apple-darwin").unwrap();
        assert_eq!(
            (a.kind.as_str(), a.protocol.as_str()),
            ("system-package", "softwareupdate")
        );
        assert_eq!(a.label_prefix, "Command Line Tools for Xcode");
        assert!(a.elevated);
        assert_eq!(
            a.provides,
            vec!["/Library/Developer/CommandLineTools/usr/bin/git".to_string()]
        );
        assert!(a.url.is_empty() && a.sha256.is_empty() && a.size == 0);
        let idx_body = full_index().replace(
            "[programs.gh]\nrepo = \"gh\"",
            "[programs.gh]\nrequires = [\"trust\", \"ay\"]\nrepo = \"gh\"",
        );
        let idx = parse_index(&verified(&idx_body)).unwrap();
        assert_eq!(
            idx.program("gh").unwrap().requires,
            vec!["trust".to_string(), "ay".to_string()]
        );
        assert!(idx.program("ay").unwrap().requires.is_empty());
    }

    /// The index-level `requires` relation is validated at parse (§17.10): a name the
    /// index does not carry, a self-dependency, a cycle over programs, and a cycle THROUGH
    /// a coherence group are each refused with the edge named — while a dependency on an
    /// extra, and one between two members of the same group, parse fine.
    #[test]
    fn the_requires_relation_is_validated_at_index_parse() {
        let with = |gh: &str, ay: &str, trust: &str| {
            full_index()
                .replace(
                    "[programs.gh]\nrepo = \"gh\"",
                    &format!("[programs.gh]\n{gh}repo = \"gh\""),
                )
                .replace(
                    "[programs.ay]\nrepo = \"ay\"",
                    &format!("[programs.ay]\n{ay}repo = \"ay\""),
                )
                .replace(
                    "[programs.trust]\nrepo = \"trust\"",
                    &format!("[programs.trust]\n{trust}repo = \"trust\""),
                )
        };
        let refused = |body: String| match parse_index(&verified(&body)) {
            Err(Reject::Requires(why)) => why,
            other => panic!("expected Reject::Requires, got {other:?}"),
        };
        // Unknown name.
        assert_eq!(
            refused(with("requires = [\"dotfiles\"]\n", "", "")),
            "gh requires dotfiles, which the index does not name"
        );
        // Self-dependency.
        assert_eq!(
            refused(with("requires = [\"gh\"]\n", "", "")),
            "gh requires itself"
        );
        // A cycle over programs, spelled out.
        let why = refused(with("requires = [\"ay\"]\n", "requires = [\"gh\"]\n", ""));
        assert!(
            why == "requires cycle: ay → gh → ay" || why == "requires cycle: gh → ay → gh",
            "{why}"
        );
        // A cycle THROUGH a coherence group: `ay` joins trust's `rustc` tuple; trust
        // requires gh, gh requires ay — the tuple would wait on itself.
        let body = with(
            "requires = [\"ay\"]\n",
            "coherence_group = \"rustc\"\n",
            "requires = [\"gh\"]\n",
        );
        let why = refused(body);
        assert!(
            why.starts_with("requires cycle through coherence group 'rustc': "),
            "{why}"
        );
        assert!(
            why.contains("gh → ay") || why.contains("trust → gh"),
            "{why}"
        );
        // Allowed: a dependency on an EXTRA (the consent surfaces name it), and one
        // between two members of ONE group (the tuple's transaction satisfies it).
        let ok = with(
            "requires = [\"codex\"]\n",
            "coherence_group = \"rustc\"\nrequires = [\"trust\"]\n",
            "",
        );
        let idx = parse_index(&verified(&ok)).expect("an extra dep and an intra-group dep parse");
        assert_eq!(
            idx.program("gh").unwrap().requires,
            vec!["codex".to_string()]
        );
        assert!(idx.is_extra("codex"));
        assert!(
            !idx.installable(&[], &[]).contains("codex"),
            "a dependency on an extra never opts it in"
        );
    }

    /// The retired `kind = "vendor-fetch"` is refused at parse with the split spelled out
    /// — never read as a binary, never as an https row, never silently Unknown.
    #[test]
    fn the_retired_vendor_fetch_kind_is_refused_naming_the_split() {
        let body = "schema = 2\nprogram = \"claude\"\nbuild_number = 1\n[[artifact]]\n\
                    target = \"aarch64-apple-darwin\"\nkind = \"vendor-fetch\"\n\
                    asset = \"claude\"\nsha256 = \"d\"\nurl = \"https://downloads.claude.ai/c\"\n";
        match parse_pkg(&verified(body)) {
            Err(Reject::RetiredKind(why)) => {
                assert!(why.contains("vendor-fetch"), "{why}");
                assert!(why.contains("kind = \"binary\""), "{why}");
                assert!(why.contains("protocol = \"https\""), "{why}");
            }
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

    /// `pkg` and `system-pm` rows parse with their own keys and WITHOUT the release-shaped
    /// ones (`asset`, and for `system-pm` `sha256`) — while a release-shaped row still
    /// fails closed without `asset`/`sha256`, exactly as when serde required them.
    #[test]
    fn pkg_and_system_pm_rows_parse_and_release_rows_still_need_their_digests() {
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
        assert_eq!(p.signer_team, "927JGANW46");
        assert!(p.elevated);
        assert_eq!(p.provides, vec!["/opt/homebrew/bin/brew".to_string()]);
        assert!(p.asset.is_empty(), "a pkg row names no release asset");
        let apt = m.artifact_for("x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(
            (
                apt.kind.as_str(),
                apt.protocol.as_str(),
                apt.manager.as_str()
            ),
            ("system-package", "system-pm", "apt")
        );
        assert_eq!(apt.package, "emacs");
        assert!(apt.elevated);
        assert!(
            apt.sha256.is_empty() && apt.url.is_empty() && apt.size == 0,
            "no bytes, no digest"
        );
        let win = m.artifact_for("x86_64-pc-windows-msvc").unwrap();
        assert_eq!(
            (win.manager.as_str(), win.package.as_str()),
            ("winget", "GNU.Emacs")
        );
        assert!(!win.elevated);
        // A target with no row is the clean skip, never an error.
        assert!(m.artifact_for("aarch64-pc-windows-msvc").is_none());
        // Release-shaped rows keep their required digests (the pre-split serde contract).
        for missing in [
            "target = \"aarch64-apple-darwin\"\nsha256 = \"d\"\n",
            "target = \"aarch64-apple-darwin\"\nasset = \"a\"\n",
            "target = \"aarch64-apple-darwin\"\nprotocol = \"https\"\nkind = \"binary\"\n\
             sha256 = \"d\"\nurl = \"https://github.com/x\"\n",
            "target = \"aarch64-apple-darwin\"\nprotocol = \"pkg\"\nkind = \"installer-pkg\"\n\
             sha256 = \"d\"\n",
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

    /// The six targets, in the schema's spelling — every one a `<arch>-<vendor>-<os>[-abi]`
    /// triple over the two architectures and three operating systems the client ships on.
    #[test]
    fn the_target_list_is_the_six_triples() {
        assert_eq!(TARGETS.len(), 6);
        for t in TARGETS {
            let arch = t.split('-').next().unwrap();
            assert!(matches!(arch, "aarch64" | "x86_64"), "{t}");
            assert!(
                t.ends_with("-apple-darwin")
                    || t.ends_with("-unknown-linux-gnu")
                    || t.ends_with("-pc-windows-msvc"),
                "{t}"
            );
        }
        let mut sorted = TARGETS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 6, "no duplicates");
    }

    /// `[programs.<name>].unavailable_hint` parses beside `extra`/`system`; absent ⇒ None.
    #[test]
    fn parses_the_unavailable_hint() {
        let mut body = full_index();
        body = body.replace(
            "[programs.gh]\nrepo = \"gh\"",
            "[programs.gh]\nunavailable_hint = \"gh is a macOS/Linux member\"\nrepo = \"gh\"",
        );
        let idx = parse_index(&verified(&body)).unwrap();
        assert_eq!(
            idx.program("gh").unwrap().unavailable_hint.as_deref(),
            Some("gh is a macOS/Linux member")
        );
        assert_eq!(idx.program("ay").unwrap().unavailable_hint, None);
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

    // A malformed / incomplete index (missing a required field) fails closed.
    #[test]
    fn malformed_index_fails_closed() {
        // Missing index_build (required) → Malformed, not a default-0 silent accept.
        let body = "schema = 2\nvalid_until = \"2026-07-05T12:00:00Z\"\n";
        assert_eq!(parse_index(&verified(body)).unwrap_err(), Reject::Malformed);
    }

    // A real TOML parser rejects a DUPLICATE key (the Phase-1 line-scanner had to
    // hand-defend this; toml gives it for free) → fail closed. `machine_id` is the one
    // that matters now: a last-wins parser would let a second copy re-attribute the index.
    #[test]
    fn duplicate_attribution_key_fails_closed() {
        let body = "schema = 2\nindex_build = 1\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
                    machine_id = \"m3\"\nmachine_id = \"m11\"\nroster_seq = 3\n";
        assert_eq!(parse_index(&verified(body)).unwrap_err(), Reject::Malformed);
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

    // A wrong-typed attribution is a hard parse failure, never a silent default: a
    // `roster_seq` that is a string cannot become "absent" and slide into the bind.
    #[test]
    fn wrongly_typed_attribution_fails_closed() {
        let body = "schema = 2\nindex_build = 1\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
                    machine_id = \"m3\"\nroster_seq = \"3\"\n";
        assert_eq!(parse_index(&verified(body)).unwrap_err(), Reject::Malformed);
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

    // The parser NEVER runs on unverified bytes: a tampered pkg fails verify, so there is
    // no VerifiedBytes to parse, and PARSE_CALLS stays flat. (The compile-time half —
    // parse_index(raw_bytes) does not type-check — needs no test.)
    #[test]
    fn parser_never_runs_on_failed_verify() {
        PARSE_CALLS.with(|c| c.set(0));
        let roster = testkit::trusted_roster();
        let body = full_index().into_bytes();
        let mut sig = testkit::sign(&testkit::MACHINE_SEED, &body);
        sig[0] ^= 0x01; // tamper
        assert!(roster.authorize_bytes(body.clone(), &sig).is_err());
        assert_eq!(
            PARSE_CALLS.with(std::cell::Cell::get),
            0,
            "no parse may run when verification fails"
        );
        // The same holds for a signature by a key NO machine on the roster holds — the
        // case the delegation tier used to answer and the roster answers now.
        let outsider = testkit::sign(&testkit::OUTSIDER_SEED, &body);
        assert!(roster.authorize_bytes(body, &outsider).is_err());
        assert_eq!(PARSE_CALLS.with(std::cell::Cell::get), 0);
        // Positive control: a good signature verifies, and only then does the parser run.
        let vb = verified(&full_index());
        let _ = parse_index(&vb).unwrap();
        assert!(PARSE_CALLS.with(std::cell::Cell::get) >= 1);
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

    // §17: `requires` is SIGNED metadata parsed from the verified bytes; absent ⇒ empty.
    #[test]
    fn parses_requires_field() {
        let with = parse_pkg(&verified(
            "schema = 2\nprogram = \"ay\"\nbuild_number = 18\nrequires = [\"ny\"]\n",
        ))
        .unwrap();
        assert_eq!(with.requires, vec!["ny".to_string()]);
        let without = parse_pkg(&verified(
            "schema = 2\nprogram = \"ay\"\nbuild_number = 18\n",
        ))
        .unwrap();
        assert!(
            without.requires.is_empty(),
            "absent requires defaults to empty"
        );
    }
}
