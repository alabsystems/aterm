// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The end-to-end install orchestration (§5/§7/§8/§9) — composing every verified
//! primitive into one program install, with the **network abstracted behind
//! [`Fetcher`]** so the whole sequence is unit-testable against synthetic *signed*
//! fixtures (no real release needed). The production [`Fetcher`] is a thin adapter over
//! `aterm-update-core`'s authenticated `curl` plumbing.
//!
//! The ordered, fail-closed pipeline ([`install`]):
//! 1. fetch index candidates (index + the master-signed roster published beside it) →
//!    [`crate::select::select_index`] (admit-roster-then-verify-then-select, §5);
//! 2. **reachability** — the program must be named in the verified index (§5), and pinned
//!    in the requested channel;
//! 3. [`crate::gate::decide`] — `UpToDate`/`Tombstone`/`NotPinned` short-circuit;
//! 4. fetch the per-build `pkg.toml`, verify it under the SAME roster generation that
//!    authorized the index ([`TrustedIndex::verify_pkg`] — revoked and expired machines
//!    already excluded), [`parse_pkg`], and check its signed `program`/`build_number`
//!    bind the request (anti-replay, §4.2);
//! 5. select the artifact for the target triple (a missing triple is a clean skip, §6);
//! 6. download → [`crate::install::verify_and_stage`] (sha256 → extract → tree_root);
//! 7. [`crate::activate::activate_channel`] + the `bin/` shim install
//!    ([`crate::activate::install_shims`], entered here through its already-admitted half
//!    because the flow needs the admitted tool set for the bundle resolve check); record.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[cfg(test)]
use crate::activate::install_tools;
use crate::activate::{Aliases, activate_channel, install_tombstone_shim, install_tools_env};
use crate::apply::{Group, TxnOutcome, plan_groups, transact};
use crate::gate::{ApplyDecision, decide};
use crate::install::StageError;
use crate::manifest::{Channel, Index, parse_pkg};
use crate::select::{Candidate, select_index};
use crate::sig::{Anchor, BuildFloor, TrustedIndex};
use crate::store::{Layout, ToolName};

/// [`crate::install::verify_and_stage`], plus the sidecar discipline BOTH stage lanes owe.
/// It deliberately takes the PLAIN name in this module — the import above is dropped — so
/// the singleton lane and `stage_member` cannot drift apart on it.
///
/// Both lanes record the signed `shim_env` beside the build (`<build>.shim-env`, design
/// S7) BEFORE the stage, so a build is never marked ready with shims whose environment
/// could not be written — and `write_sidecar` creates `store/<program>/` in order to do
/// it. A stage that then FAILS used to walk away leaving that sidecar beside a build
/// directory that never came to exist, and nothing on the machine ever reclaimed it:
/// `gc::interrupted_debris` enumerates `store/<program>/` for DIRECTORIES only, and
/// `store::discard_build` — the one verb that takes a sidecar away — is only ever reached
/// with a tree in hand. MEASURED 2026-09-13: after the refused pass `store/claude/` held
/// exactly one entry, `2026091301.shim-env`, and `atpkg gc` reported nothing to reclaim.
///
/// This reclaims the FILE, not the directory, and the distinction is honest: the stage
/// creates `store/<program>/` itself for its `<build>.incoming-<pid>` scratch on every
/// attempt that gets past the sha256 gate, and nothing sweeps an empty program directory
/// — so a program that never installed can still leave one standing. What is closed here
/// is the unreclaimable sidecar, which is the part gc could never have reached.
///
/// The existence test is the load-bearing half. A re-stage OVER a live build fails with
/// the old tree still in place (the stage builds beside it and swaps), and that tree's
/// shims still need the environment just written for its build number — so a sidecar with
/// a build under it is never touched here. A tree that exists but is incomplete is
/// likewise left alone: it is gc's partial, and `discard_build` takes its sidecar with it.
fn verify_and_stage(
    artifact: &crate::manifest::Artifact,
    archive: &Path,
    build_dir: &Path,
) -> Result<(), StageError> {
    let staged = crate::install::verify_and_stage(artifact, archive, build_dir);
    if staged.is_err() && !build_dir.exists() {
        crate::shim_env::remove_sidecar(build_dir);
    }
    staged
}

/// The network operations the install flow needs, abstracted so the orchestration is
/// testable. The production impl wraps `aterm-update-core`'s `api_get`/`download_to`.
pub trait Fetcher {
    /// The candidate `index.toml`+sig assets across recent releases of the index repo.
    fn index_candidates(&self) -> Result<Vec<Candidate>, String>;
    /// The `(pkg-<program>-<build>.toml, .sig)` raw bytes for a resolved build, from the
    /// program's own `repo` (the index's `[programs.<name>].repo`, §4.2).
    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String>;
    /// Download `asset` (from the program's `repo`) to `dest`.
    fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String>;
    /// [`Fetcher::download`], additionally told WHICH program the asset belongs to and
    /// the row's SIGNED `size` as the byte `cap`.
    /// Default: identical to `download`. The production GitHub fetcher overrides
    /// this to honor a per-program `[packages.links]` `owner/repo` FETCH override
    /// (a possibly-private repo the program's release assets are pulled from).
    /// The host is never an authenticity input (§5/§8): the bytes still pass the
    /// identical signed-manifest sha256 + `tree_root` gates, so a redirected
    /// fetch can only supply bytes, not trust. The flow calls THIS for artifact
    /// downloads so the override reaches both the install and the stage paths.
    ///
    /// `cap` is the row's signed `size`, exactly as [`Fetcher::download_url`] takes it,
    /// because it is the same number the disk preflight was computed from
    /// (`disk_gate(size + disk_installed)`). That preflight runs once, BEFORE the
    /// transfer, and the sha256 gate runs only over the COMPLETE file — so a lane capped
    /// at anything larger writes those extra bytes into `staging/`, through the
    /// free-space floor the preflight had just defended, and refuses them afterwards
    /// (§9, "caps from the SIGNED manifest, not the API"). `0` means the row states no
    /// size ([`crate::vendor::check_row`] requires one only for the `https`/`pkg`
    /// protocols) and the fetcher falls back to its own ceiling.
    ///
    /// The DEFAULT drops it with `download`'s signature, which carries no cap: that is
    /// the `dir:` registry lane, which hardlinks or copies a file already on this disk —
    /// there is no transfer to bound and no host to distrust.
    fn download_for(
        &self,
        program: &str,
        repo: &str,
        asset: &str,
        dest: &Path,
        cap: u64,
    ) -> Result<(), String> {
        let _ = (program, cap);
        self.download(repo, asset, dest)
    }
    /// Download an arbitrary SIGNED `https` `url` to `dest`, capped at EXACTLY `cap`
    /// bytes — the `https` protocol lane (`fetch_artifact`, keyed on the row's `protocol`).
    /// `cap` is the row's signed `size`, so a vendor re-release under the same URL fails
    /// fast on the cap or the sha256 and can never inflate the download.
    ///
    /// DEFAULT: REFUSED. Fail closed so a `dir:` registry or a test fetcher never reaches
    /// the network by accident — only the production network fetcher opts in, and it
    /// presents NO credential to a vendor host (the GitHub token never leaves GitHub).
    fn download_url(&self, url: &str, dest: &Path, cap: u64) -> Result<(), String> {
        let _ = (url, dest, cap);
        Err(
            "this fetcher cannot fetch vendor URLs (the https protocol is network-only)"
                .to_string(),
        )
    }
    /// Canonical identity of this fetcher's source (`github:<owner>/<repo>` or `dir:<path>`),
    /// tagging the index cache so a cache from one source never satisfies a failed fetch from
    /// another (the same-source guard, §14). The default is a non-matching sentinel, so a test
    /// fetcher opts out of cross-run cache reuse.
    fn source_id(&self) -> String {
        "fetcher:unspecified".to_string()
    }
    /// The §14 last-good-index cache key this fetcher's candidates are stored under AND
    /// loaded from on a failed/empty fetch. Default: [`Fetcher::source_id`]. A chained
    /// fetcher ([`crate::net::ChainFetcher`]) narrows it to its PRIMARY (network) leg's
    /// key, so the cache identity is the same whether or not a seed leg happens to be
    /// chained: the bootstrap-time cache must serve the post-bootstrap plain-network
    /// path, and a `dir:` seed cache must never satisfy it (the same-source guard).
    fn cache_source_id(&self) -> String {
        self.source_id()
    }
    /// The subset of `resolved` — THIS call's just-returned [`Fetcher::index_candidates`]
    /// success — the §14 cache may persist, or `None` to skip the write. Default: all of
    /// it (a single-source fetcher IS its network leg). [`crate::net::ChainFetcher`]
    /// overrides this to the primary (network) leg's own candidates: a seed-leg success
    /// must neither mask a network failure into a cache refresh nor overwrite the
    /// last-good network candidates with sealed-seed bytes (the CACHE-MASKING tooth,
    /// adversarial review 2026-07-30).
    fn cacheable_candidates(&self, resolved: &[Candidate]) -> Option<Vec<Candidate>> {
        Some(resolved.to_vec())
    }
    /// A CHEAP fingerprint of each candidate [`Fetcher::index_candidates`] would return
    /// THIS pass, in the SAME order — or `None` (the default) when this fetcher cannot
    /// answer without doing the expensive work.
    ///
    /// # What this is for
    ///
    /// `index_candidates` is the most expensive thing a no-op update does. The production
    /// fetcher spends one listing request plus FOUR asset downloads per candidate — with
    /// `INDEX_CANDIDATE_CAP = 4`, sixteen sequential `curl` subprocesses each paying its
    /// own DNS+TLS handshake — and the §14 cache beside it held those very bytes but was
    /// consulted only when the fetch FAILED. Steady state therefore re-downloaded, every
    /// pass and in every fresh process, bytes it could prove it already had.
    ///
    /// The saving is ROUND-TRIPS, not rate-limit budget, and the difference matters: the
    /// sixteen asset reads already go to the unmetered release CDN, and the listing — one
    /// `api.github.com` request PER PAGE on the listing lane — still happens there. (On the
    /// pointer lane, `crate::net::GithubFetcher::index_pointer`, discovery is one
    /// unmetered HEAD and there is no API request to save at all.) Claiming the anonymous
    /// 60/hr budget back would be describing the code as it was before the zero-API asset
    /// fetch landed.
    ///
    /// This method is the proof. The production impl derives it from the release LISTING
    /// (which `index_candidates` must fetch anyway, and memoizes), so answering costs
    /// nothing beyond the request that was always going to happen — and the listing
    /// already carries each asset's identity, because a GitHub asset URL names an id that
    /// changes if and only if the asset is re-uploaded.
    ///
    /// # Contract
    ///
    /// * SAME ORDER, SAME LENGTH as `index_candidates` would return this pass. The
    ///   position is the pairing; [`crate::IndexCache::store`] drops the whole vector on
    ///   a length disagreement rather than zipping a prefix.
    /// * A fingerprint must change whenever ANY of the four assets behind that candidate
    ///   changes. It need not be unforgeable — see the module doc of [`crate::cache`]:
    ///   equality only decides whether to re-download bytes that face every trust gate
    ///   either way.
    /// * `None` or an empty vector means "no cheap answer", which is always safe: the
    ///   caller then takes the historical download path.
    ///
    /// # Why the default is `None` — and why [`crate::net::ChainFetcher`] keeps it
    ///
    /// A fetcher that opts in is telling the resolver it may skip `index_candidates`
    /// entirely. For a chained fetcher that would skip the SEED leg too, because the §14
    /// cache holds only the network leg's candidates ([`Fetcher::cacheable_candidates`]) —
    /// and the seed union is exactly what the 2026-07-30 cache-masking review put there.
    /// The chain is joined only on an EMPTY store (one bootstrap pass), so opting it in
    /// would trade the crate's most adversarially-reviewed property for nothing.
    fn index_identities(&self) -> Option<Vec<String>> {
        None
    }
}

/// What an install did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    /// The program installed.
    pub program: String,
    /// The build it is now on.
    pub build: u64,
    /// The `index_build` of the index this install resolved from — the caller advances the
    /// durable high-water [`crate::sig::Floor`] to this on success (§8 gate 3).
    pub index_build: u64,
    /// The `roster_seq` of the master-signed generation that authorized that index. The
    /// caller ratchets the durable ROSTER floor to this on success, which is what makes a
    /// replayed generation refusable forever after. Carried beside `index_build` rather
    /// than derived from it because the two documents move independently: a roster bump
    /// (mint, revoke) does not re-cut the index, and an index re-publish does not bump the
    /// roster.
    pub roster_seq: u64,
    /// Whether it was already current (no change).
    pub already_current: bool,
    /// The `bin/` shims installed.
    pub shimmed: Vec<String>,
    /// `exposes` names refused a shim (sensitive-name collisions, §10).
    pub refused_shims: Vec<String>,
    /// The SIGNED `tree_root` (§8) of the installed build, copied from the release-key-
    /// verified manifest artifact. The CLI records this into `status.toml` so `atpkg verify`
    /// can re-attest the on-disk tree against the signed root (never a self-generated hash).
    /// Empty when the program was already current (no fetch) or the manifest omitted one.
    pub tree_root: String,
    /// Required-dependency pull-in outcomes (flattened transitive closure), each
    /// installed-first, with yanked/unreachable/cyclic deps SKIPPED (never gate-bypassing).
    pub dependencies: Vec<DepOutcome>,
    /// `Some` when the member was applied through an OS-installer protocol (`pkg`,
    /// `softwareupdate`, `system-pm`) instead of the store: what that lane did — proven
    /// present at a `provides` path, DEFERRED for want of elevation, or UNAVAILABLE here
    /// (a `system-pm` row whose manager this machine lacks). Nothing was staged, shimmed or
    /// activated for such a member (`shimmed` is empty, `tree_root` empty), and the
    /// caller records the outcome's canonical state ([`ProtocolOutcome::state`]) rather
    /// than `managed <build>`. `None` for every store-managed install.
    pub protocol: Option<ProtocolOutcome>,
}

/// What an OS-installer lane did for a member — the three states such a member can be
/// in after a pass, in the canonical words ([`crate::state`]). The `protocol` each
/// carries is the spelling the state prints: `pkg`, `softwareupdate`, or — for a
/// `system-pm` row — the MANAGER's name (`apt`, `brew`, `winget`, …), since that is
/// what keeps the member current from then on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolOutcome {
    /// Present: a `provides` path exists — installed this pass, or found already there
    /// (the row's `provides` IS the system copy; nothing is ever re-downloaded for it).
    /// Recorded as `installed via <protocol>: <path>`.
    Installed {
        /// The protocol (or manager) that proved it (`pkg`, `softwareupdate`, `apt`, …).
        protocol: &'static str,
        /// The first `provides` path that exists.
        path: PathBuf,
    },
    /// Not present, and this pass may not elevate ([`crate::elevate::Elevation::Deferred`]
    /// — the unattended pass, or a door with no terminal). Recorded as `needs admin —
    /// run: aterm pkg install <name>`; the explicit door is where it installs.
    NeedsAdmin {
        /// The protocol (or manager) that is waiting.
        protocol: &'static str,
    },
    /// Not present, and not installable HERE: a `system-pm` row whose manager is not on
    /// this machine's `PATH`. Recorded as `unavailable on <target>: <hint>` — a state,
    /// never a fault; atpkg never installs a package manager, so nothing waits on the
    /// door either ([`crate::system_pm::missing_manager_hint`] spells the hint).
    Unavailable {
        /// The manager that is missing.
        protocol: &'static str,
        /// The target triple the row was pinned for.
        target: String,
        /// The hint the state carries.
        hint: String,
    },
}

impl ProtocolOutcome {
    /// The canonical state row for `program`.
    #[must_use]
    pub fn state(&self, program: &str) -> String {
        match self {
            ProtocolOutcome::Installed { protocol, path } => {
                crate::state::installed_via(protocol, path)
            }
            ProtocolOutcome::NeedsAdmin { .. } => crate::state::needs_admin(program),
            ProtocolOutcome::Unavailable { target, hint, .. } => {
                crate::state::unavailable(target, hint)
            }
        }
    }

    /// The protocol's (or manager's) spelling.
    #[must_use]
    pub fn protocol(&self) -> &'static str {
        match self {
            ProtocolOutcome::Installed { protocol, .. }
            | ProtocolOutcome::NeedsAdmin { protocol }
            | ProtocolOutcome::Unavailable { protocol, .. } => protocol,
        }
    }

    /// Whether the member is waiting on elevation.
    #[must_use]
    pub fn is_deferred(&self) -> bool {
        matches!(self, ProtocolOutcome::NeedsAdmin { .. })
    }

    /// Whether the member cannot be obtained on this machine at all (its manager is
    /// absent) — for an OS-installed parent, a requirement that fails, not one that waits.
    #[must_use]
    pub fn is_unavailable(&self) -> bool {
        matches!(self, ProtocolOutcome::Unavailable { .. })
    }
}

/// One resolved `requires` dependency and what happened to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepOutcome {
    /// The dependency program name.
    pub program: String,
    /// The pull-in result.
    pub result: DepResult,
}

/// The outcome of resolving one `requires` dependency. A skip is always safe: a `requires`
/// edge pulls a program IN, never bypasses a gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepResult {
    /// Freshly installed at `build`, carrying its SIGNED `tree_root` so the CLI records it.
    Installed {
        /// The build the dependency was installed at.
        build: u64,
        /// The dependency's signed `tree_root`.
        tree_root: String,
    },
    /// Already active at `build` — left as-is (its own status was recorded at its install).
    AlreadyPresent(u64),
    /// Satisfied by a SYSTEM install ([`crate::manifest::Program::system`]): the binary
    /// is on `PATH` outside the managed prefix, so nothing is installed for it — the
    /// owner's rule 2, kept on the `requires` path (a pull-in must never lay a managed
    /// copy beside the user's own). Carries the path; the caller records the canonical
    /// `system: <path> — not managed by aterm` row if it changed.
    System(PathBuf),
    /// Applied through an OS-installer protocol: present at a `provides` path, or
    /// DEFERRED for want of elevation — in which case a parent that is itself an
    /// OS-installed member defers too (its installer needs the dependency first).
    Protocol(ProtocolOutcome),
    /// Skipped, with a reason (unreachable / tombstoned / not pinned / cycle / fetch error).
    Skipped(String),
}

/// Every program name a VERIFIED index carries, for [`FlowError::NotReachable`]'s
/// "it names: …" answer. `BTreeMap` keys, so the order is stable and alphabetical.
fn index_roster(index: &crate::manifest::Index) -> Vec<String> {
    index.programs.keys().cloned().collect()
}

/// Why an install failed, fail-closed at each gate.
#[derive(Debug)]
pub enum FlowError {
    /// No signature-valid index at/above the floor was found.
    NoIndex,
    /// The index SOURCE could not be reached at all — offline, DNS failure, a proxy,
    /// a 403 rate-limit — and no §14 cached index was available to stand in.
    ///
    /// Distinct from [`FlowError::NoIndex`] because the two need opposite reactions
    /// and used to be indistinguishable: a transport failure fell into the `_` arm of
    /// `resolve_candidates`, which discarded the reason and reported "no
    /// signature-valid index at/above the floor". That is a TRUST verdict, and it is
    /// the first thing an offline machine showed its owner — sending someone to key
    /// management when the fix is "connect to the internet", and quietly implying the
    /// publisher's signatures are bad.
    Unreachable(String),
    /// The program is not named in the verified index (unreachable, §5). The second
    /// field is the roster the index DOES name, captured at construction — the one
    /// moment the just-verified index is in hand — so the error can answer the user's
    /// immediate next question ("then what IS installable?") without any surface
    /// re-fetching, and without any trust change: the printed roster is
    /// master-root-verified. Empty means "roster unknown" and the message stays short.
    NotReachable(String, Vec<String>),
    /// The requested channel does not exist in the index.
    NoChannel(String),
    /// The program is not pinned in the channel.
    NotPinned(String),
    /// The selected index's freshness window has lapsed (`now >= valid_until`, §8).
    Stale,
    /// The pinned build is yanked/below-floor — tombstoned, nothing installed (§7).
    Tombstoned(String),
    /// Fetching the per-build manifest failed.
    PkgFetch(String),
    /// The per-build manifest did not verify under the delegated release key.
    PkgVerify,
    /// The per-build manifest was malformed / a newer schema.
    PkgParse,
    /// The per-build manifest spells a RETIRED `kind` (`vendor-fetch`); carries the
    /// split that replaced it ([`crate::Reject::RetiredKind`]) so the refusal names the
    /// fix.
    RetiredKind(&'static str),
    /// The per-build manifest's `shim_env` breaks the rule a shim can honour
    /// ([`crate::Reject::ShimEnv`], design S7); carries the entry and the reason, like
    /// `RetiredKind`, so the authoring machine's own install names the fix.
    ShimEnv(String),
    /// The signed `program`/`build_number` did not match the request (anti-replay).
    Mismatch,
    /// No artifact for the target triple (a clean skip, §6).
    NoArtifact(String),
    /// The artifact's `kind` is not installable by this tool path — an unrecognized kind.
    /// Fail-closed (§16.4 dispatch).
    UnsupportedKind(String),
    /// An `app-bundle` member was refused by the two-anchor app-apply gate
    /// ([`crate::appgate::app_apply_allowed`], §16.2/§16.4). The app is applied in-session by
    /// its own updater (aterm-gui's overlap handoff), a topology the CLI tool-install path
    /// deliberately does not carry, so the gate's unconditional notarization AND-anchor is
    /// unproven here and the decision fails closed. atpkg never swaps the app (see
    /// `app_apply_gate_refused` for the deferral rule).
    AppBundleRefused(String),
    /// An artifact row failed per-protocol admission ([`crate::vendor::check_row`]) BEFORE
    /// any byte moved: for `https`, a non-https or non-allow-listed `url`, an unknown
    /// `payload`, a raw-binary `entry` that is not an exposed shimmable name, a hostile
    /// `links` target, or a missing signed digest; for `pkg`/`system-pm`, their own field
    /// rules. The message names the field.
    VendorRefused(String),
    /// An OS-installer lane (`pkg`, `softwareupdate`, `system-pm`) refused or failed AFTER
    /// admission and after the elevation decision: a package signed outside the pinned
    /// team, an installer or manager that exited non-zero, a `softwareupdate -l` with no
    /// label, or an install that left none of the `provides` paths behind. `protocol` is
    /// the lane's spelling (the manager's name for `system-pm`); `why` names the reason.
    Protocol { protocol: &'static str, why: String },
    /// A `requires` entry of an OS-installed member could not be satisfied first (the
    /// installer would only fail later, less legibly — Homebrew's `.pkg` refuses to
    /// install without the Command Line Tools). Names the dependency and its outcome.
    Requirement { dep: String, why: String },
    /// The asset download failed.
    Download(String),
    /// Staging (sha256 / extract / tree_root) failed.
    Stage(StageError),
    /// The pinned build's SIGNED digests are the ones a recent attempt already proved the
    /// published asset does not match, and the cooldown on that refusal has not lapsed —
    /// so this pass did not re-download the asset to reach the same verdict
    /// ([`crate::store::StageRefusal`]). Carries the recorded sentence, which names the
    /// original stage failure and when the next attempt is due. Nothing was fetched,
    /// staged or changed.
    StageRefused(String),
    /// Activation / shim install failed.
    Activate(String),
    /// Rollback-specific failure: the program is not active, or no retained build below
    /// current satisfies the floor/yank gate.
    Rollback(String),
    /// Disk preflight failed: the volume lacks room for the artifact + its extracted tree
    /// while leaving the free floor (§9). Nothing was downloaded/staged.
    InsufficientDisk { required: u64, available: u64 },
    /// The program is dev-linked (linkmode, §13) — `update`/`apply` HARD-SKIPS it until
    /// `atpkg unlink`.
    Linked(String),
}

// Hand-rendered through `Formatter::write_str` + direct `Display::fmt`/`Debug::fmt`
// calls (no `write!`) — Trust-gate lowering workaround, see `lib.rs`. Byte-identical
// to the `write!` forms (no width/fill flags are used).
impl std::fmt::Display for FlowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlowError::NoIndex => f.write_str("no signature-valid index at/above the floor"),
            FlowError::Unreachable(why) => {
                f.write_str("could not reach the toolchain index (")?;
                f.write_str(why)?;
                // No "…the toolchain retries automatically" tail: that is true only of
                // the windowed app's 6-hour loop, and this Display reaches foreground
                // CLI verbs where nothing retries anything. The CLI edge appends the
                // honest per-verb re-run instead (`print_unreachable_followup`).
                f.write_str(") — this is a network problem, not a signature problem")
            }
            FlowError::NotReachable(p, roster) => {
                f.write_str(p)?;
                // The grep-stable phrase, verbatim; the roster rides in a parenthesis.
                f.write_str(" is not named in the signed index")?;
                if roster.is_empty() {
                    return Ok(());
                }
                f.write_str(" (it names: ")?;
                // Whole roster when it is short; first 12 otherwise — the point is a
                // usable answer, not a wall.
                if roster.len() <= 12 {
                    f.write_str(&roster.join(", "))?;
                } else {
                    f.write_str(&roster[..12].join(", "))?;
                    f.write_str(", …")?;
                }
                f.write_str(")")
            }
            FlowError::NoChannel(c) => {
                f.write_str("no channel ")?;
                std::fmt::Debug::fmt(c, f)?;
                f.write_str(" in the index")
            }
            FlowError::NotPinned(p) => {
                f.write_str(p)?;
                f.write_str(" is not pinned in the channel")
            }
            FlowError::Stale => f.write_str("the signed index's freshness window has lapsed"),
            FlowError::Tombstoned(p) => {
                f.write_str(p)?;
                f.write_str("'s pinned build is yanked/below floor")
            }
            FlowError::PkgFetch(e) => {
                f.write_str("fetch manifest: ")?;
                f.write_str(e)
            }
            FlowError::PkgVerify => f.write_str("manifest signature did not verify"),
            FlowError::PkgParse => f.write_str("manifest malformed or newer schema"),
            FlowError::RetiredKind(why) => {
                f.write_str("manifest refused: ")?;
                f.write_str(why)
            }
            FlowError::ShimEnv(why) => {
                f.write_str("manifest refused: ")?;
                f.write_str(why)
            }
            FlowError::Mismatch => f.write_str("manifest program/build did not match the request"),
            FlowError::NoArtifact(t) => {
                f.write_str("no artifact for target ")?;
                f.write_str(t)
            }
            FlowError::UnsupportedKind(k) => {
                f.write_str("artifact kind ")?;
                std::fmt::Debug::fmt(k, f)?;
                f.write_str(" is not installable by `atpkg install`")
            }
            FlowError::AppBundleRefused(p) => {
                f.write_str(p)?;
                f.write_str(
                    "'s app-bundle is not installed by atpkg — aterm updates itself in-session \
                     through its own notarization-gated updater (see `aterm ctl update status`); \
                     the app-apply gate fails closed here because notarization is unproven on \
                     the CLI path",
                )
            }
            FlowError::VendorRefused(why) => {
                f.write_str("artifact row refused: ")?;
                f.write_str(why)
            }
            FlowError::Protocol { protocol, why } => {
                f.write_str(protocol)?;
                f.write_str(": ")?;
                f.write_str(why)
            }
            FlowError::Requirement { dep, why } => {
                f.write_str("requires ")?;
                f.write_str(dep)?;
                f.write_str(", which could not be installed first: ")?;
                f.write_str(why)
            }
            FlowError::Download(e) => {
                f.write_str("download: ")?;
                f.write_str(e)
            }
            FlowError::Stage(e) => {
                f.write_str("stage: ")?;
                std::fmt::Display::fmt(e, f)
            }
            // No head: the recorded sentence already names the program, the build, the
            // original stage failure and the way out — a "stage: " prefix would read as
            // if this pass had staged something, and it staged nothing.
            FlowError::StageRefused(m) => f.write_str(m),
            FlowError::Activate(e) => {
                f.write_str("activate: ")?;
                f.write_str(e)
            }
            FlowError::Rollback(m) => {
                // No "rollback: " head: the one CLI edge already prints
                // "atpkg: rollback <p> failed:", and the doubled word read like two
                // errors stacked.
                f.write_str(m)
            }
            FlowError::InsufficientDisk {
                required,
                available,
            } => {
                f.write_str("insufficient disk: need ")?;
                f.write_str(&crate::cost::human_bytes(*required))?;
                f.write_str(" free (have ")?;
                f.write_str(&crate::cost::human_bytes(*available))?;
                f.write_str(")")
            }
            FlowError::Linked(p) => {
                f.write_str(p)?;
                f.write_str(" is dev-linked; run `aterm pkg unlink ")?;
                f.write_str(p)?;
                f.write_str("` to manage it from the registry")
            }
        }
    }
}

impl std::error::Error for FlowError {}

/// What to install: the `channel` to resolve the pin from, the `program`, the target
/// `triple`, and the currently-`installed` build (if any) for the up-to-date/upgrade
/// decision.
#[derive(Debug, Clone, Copy)]
pub struct InstallRequest<'a> {
    /// The channel whose pin set names the build.
    pub channel: &'a str,
    /// The program to install.
    pub program: &'a str,
    /// The target triple to select an artifact for.
    pub triple: &'a str,
    /// The currently-active build, if any (the upgrade/up-to-date input).
    pub installed: Option<u64>,
}

/// Re-write `store/<program>/current` and `channels/<channel>/current` at `build` unless
/// the prefix already proves that build live for `program` ([`crate::gc::live_builds`]).
///
/// The up-to-date path's one write. `installed` is what the shims run, so on this path
/// the pin and the shims agree — but a prefix last written by a manager older than the
/// per-program link, a `current` that dangles because its build was removed, or a channel
/// link left at a superseded build has NO witness for the program: GC abstains on it
/// forever and `doctor` names it diverged with `update` as the remedy. Until this existed,
/// that remedy did nothing, because the program was "already current". A healthy store is
/// not touched; a build that is not on disk is left for `doctor` to name.
fn reassert_witness(
    layout: &Layout,
    channel: &str,
    program: &str,
    build: u64,
) -> Result<(), FlowError> {
    if crate::gc::live_builds(layout)
        .get(program)
        .is_some_and(|w| w.build() == build)
    {
        return Ok(());
    }
    let build_dir = layout.build_dir(program, build);
    if !build_dir.is_dir() {
        return Ok(());
    }
    activate_channel(layout, channel, &build_dir).map_err(|e| FlowError::Activate(e.to_string()))
}

/// The installed-build view [`decide`] may call UP-TO-DATE: the SHIM-derived build, dropped
/// to `None` when the tree it names is not a COMPLETE install **for this slice**
/// ([`crate::store::build_is_complete`] — the sibling `<build>.ready` marker, platform
/// record and all).
///
/// WHY THE SHIM VIEW IS NOT ENOUGH. `installed` reaches every apply path from
/// [`crate::ops::active_builds`], which resolves `bin/<tool>` and stats its target — the
/// authority on what RUNS, and deliberately silent about the marker. So a LIVE build whose
/// marker is missing, or was written by the OTHER slice of the universal binary
/// (`store::running_platform`: an `arch -x86_64` pass installs INTEL trees into the very
/// same `store/<program>/<build>/`), still handed `decide` a number equal to the pin,
/// `decide` answered `UpToDate`, [`reassert_witness`] found a valid witness and returned —
/// and NOTHING ever re-staged that tree. `list_installed`, `doctor` and `gc` all read the
/// marker, so `doctor` FAILed forever ("active <p> build <n> store missing/incomplete")
/// while the structural repair it names, `install <program>`, re-entered through the same
/// shim view and printed "already current". The machine kept running the other
/// architecture's compilers and solvers under Rosetta, silently — the exact outcome the
/// platform record was added to prevent.
///
/// This is the input that makes that record's promise true: an incomplete or other-slice
/// tree reads as not-installed to `decide` as well, so the ordinary Install path re-stages
/// it in place ([`Staged::was_live`] keeps the live tree through an abort) and the marker
/// comes back native.
///
/// Strictly a DOWNGRADE of the decision — `None` can only turn `UpToDate` into `Install`,
/// never suppress a `Tombstone` (which reads the pin, not the installed build) — and
/// `build_is_complete` spends only a marker PROVEN absent (or proven another slice's) on
/// `false`, so a marker this process merely cannot read never triggers a multi-gigabyte
/// re-download.
fn installed_for_decide(layout: &Layout, program: &str, installed: Option<u64>) -> Option<u64> {
    installed.filter(|&build| crate::store::build_is_complete(&layout.build_dir(program, build)))
}

/// Install (or force-upgrade) the program named by `req`, using `fetcher` for all network
/// I/O, `anchor` as the pinned paper-master keyset + durable roster ratchet, and `floor`
/// as the durable index high-water **paired with the roster generation that recorded it**
/// ([`BuildFloor`] — a floor a machine set does not outlive the generation that revoked
/// that machine). See the module docs for the ordered, fail-closed pipeline. An unarmed
/// `anchor` installs nothing: `select_index` yields no candidate and this returns
/// [`FlowError::NoIndex`].
pub fn install(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    req: &InstallRequest,
    floor: BuildFloor,
    now_unix: i64,
) -> Result<InstallReport, FlowError> {
    install_collecting_assets(
        fetcher,
        layout,
        anchor,
        req,
        floor,
        now_unix,
        &mut BTreeMap::new(),
    )
}

/// [`install`], additionally filling `resolved_assets` with `program → pinned asset file
/// name` for every program (the requested one AND each `requires` pull-in) the pass
/// resolved far enough to select an artifact.
///
/// An OUT-PARAM, not an [`InstallReport`] field, because it must survive the `Err` path:
/// the program whose download failed mid-transfer is exactly the one whose `<asset>.part`
/// resume state the caller's pass-end [`crate::gc::run_keeping_pinned_partials`] must
/// spare — and a failed install returns no report to carry the name. The map only ever
/// SHRINKS that gc sweep (see the sparing form's trust-posture note), so over-collecting
/// on the success path is harmless: a finished install has no `.part` left to spare.
pub fn install_collecting_assets(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    req: &InstallRequest,
    floor: BuildFloor,
    now_unix: i64,
    resolved_assets: &mut BTreeMap<String, String>,
) -> Result<InstallReport, FlowError> {
    let mut seen = BTreeMap::new();
    install_inner(
        fetcher,
        layout,
        anchor,
        req,
        floor,
        now_unix,
        &mut seen,
        resolved_assets,
    )
}

/// [`install`] with a cycle-guard `seen` map threaded through the `requires` recursion (§17):
/// a dependency that requires back into a program already on the resolution stack (`None`)
/// is skipped rather than looping, and one a sibling edge already resolved in this call
/// (`Some(outcome)`, a diamond) carries that outcome to every later edge naming it.
#[allow(
    clippy::too_many_arguments,
    reason = "install_inner is install plus the recursion cycle-guard set and the resolved-\
              asset collector; every other input is an irreducible dependency of a verified \
              install (fetcher, layout, root key, the request, and the floor + clock the \
              anti-rollback/freshness gates read)"
)]
fn install_inner(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    req: &InstallRequest,
    floor: BuildFloor,
    now_unix: i64,
    seen: &mut BTreeMap<String, Option<DepResult>>,
    resolved_assets: &mut BTreeMap<String, String>,
) -> Result<InstallReport, FlowError> {
    let (channel, program, triple, installed) =
        (req.channel, req.program, req.triple, req.installed);
    // 0. Dev-link HARD-SKIP (§13): a linked program is managed from its checkout, not the
    //    registry — short-circuit before any network I/O so link composes ahead of every
    //    other gate.
    if crate::linkmode::is_linked(layout, program) {
        return Err(FlowError::Linked(program.to_string()));
    }
    seen.insert(program.to_string(), None);
    // 1–2. Resolve + verify-select the index + freshness (§8 gate 2) — the shared
    // [`resolve_verified_index`] prologue (cached-fallback, §14) — then reachability.
    let index = resolve_verified_index(fetcher, layout, anchor, floor, now_unix)?;
    let repo = index
        .program(program)
        .ok_or_else(|| FlowError::NotReachable(program.to_string(), index_roster(&index)))?
        .repo
        .clone();
    // The channel AS SEEN FROM THIS HOST: a per-target pin overlay, if the index
    // carries one for `triple`, is resolved here and nowhere else.
    let ch = index
        .channel_for(channel, triple)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    let ch = &ch;
    let &pinned = ch
        .pin
        .get(program)
        .ok_or_else(|| FlowError::NotPinned(program.to_string()))?;

    // 3. The apply decision — from the COMPLETENESS-aware view of what is installed
    //    ([`installed_for_decide`]), never the raw shim view: a LIVE build whose store tree
    //    this slice cannot vouch for must re-stage, not read as already-current.
    match decide(
        ch,
        program,
        installed_for_decide(layout, program, installed),
    ) {
        ApplyDecision::UpToDate => {
            // No fetch, no stage — but the no-op pass still owns the liveness witness.
            reassert_witness(layout, channel, program, pinned)?;
            return Ok(InstallReport {
                program: program.to_string(),
                build: pinned,
                index_build: index.index_build,
                roster_seq: index.roster_seq(),
                already_current: true,
                shimmed: vec![],
                refused_shims: vec![],
                // An up-to-date program short-circuits before its manifest is fetched, so its
                // `requires` are not resolved on a no-op install (documented gap).
                tree_root: String::new(),
                dependencies: vec![],
                protocol: None,
            });
        }
        ApplyDecision::Tombstone => {
            // Actively DISABLE the revoked build's OLD working shims (§7): install a failing
            // tombstone shim over each currently-exposed tool so a yanked/below-floor build is
            // not left runnable, rather than merely reporting Tombstoned. Best-effort — a
            // tombstone-write failure must not mask the authoritative Tombstoned error.
            install_tombstone_shims(layout, program, installed);
            return Err(FlowError::Tombstoned(program.to_string()));
        }
        ApplyDecision::NotPinned => return Err(FlowError::NotPinned(program.to_string())),
        ApplyDecision::Install => {}
    }

    // 4. Fetch + verify + parse the per-build manifest; bind program + build (anti-replay).
    let (raw, sig) = fetcher
        .pkg_manifest(&repo, program, pinned)
        .map_err(FlowError::PkgFetch)?;
    let verified = index
        .verify_pkg(raw, &sig)
        .map_err(|_| FlowError::PkgVerify)?;
    let pkg = parse_pkg(&verified).map_err(|e| match e {
        // The retired `kind = "vendor-fetch"` names the split that replaced it — the
        // one parse refusal whose words reach the authoring machine, so the fix is
        // spelled rather than read as a bare "malformed".
        crate::Reject::RetiredKind(why) => FlowError::RetiredKind(why),
        // So does a `shim_env` the rule refuses (design S7): the entry and the reason.
        crate::Reject::ShimEnv(why) => FlowError::ShimEnv(why),
        _ => FlowError::PkgParse,
    })?;
    if !pkg.is_for(program) || pkg.build_number != pinned {
        return Err(FlowError::Mismatch);
    }

    // 4b. Runtime `requires` (§17): pull in each MISSING dep FIRST, through the SAME verified
    // pipeline (reachability + freshness + floor/yank gate). Best-effort — a dep failure never
    // fails a STORE-MANAGED parent's install; a Tombstone/NotReachable is SKIPPED, never
    // bypassed. `requires` is SIGNED metadata — the index's `[programs.<name>].requires`
    // (Homebrew requires `clt`) unioned with the pkg manifest's own, both parsed from
    // verified bytes — so a repo-write adversary can neither inject nor redirect an edge.
    // (An OS-installed parent is stricter: see the requirement gate at step 5b.)
    let mut requires: Vec<String> = index
        .program(program)
        .map(|p| p.requires.clone())
        .unwrap_or_default();
    for dep in &pkg.requires {
        if !requires.contains(dep) {
            requires.push(dep.clone());
        }
    }
    let mut dependencies: Vec<DepOutcome> = Vec::new();
    // ONE `bin/` scan for the whole loop: any program this recursion installs is in
    // `seen` (inserted at entry) and screened by the check below BEFORE the map is
    // consulted, so a pre-loop snapshot decides identically.
    let active = crate::ops::active_builds(layout);
    for dep in &requires {
        if let Some(earlier) = seen.get(dep) {
            match earlier {
                // Resolved EARLIER in this call — a sibling's recursion pulled it in (a
                // diamond: brew requires [mid, clt], mid requires clt). Its outcome stands
                // for this edge too, so an OS-installed parent's requirement gate reads
                // what really happened rather than a skip. Already in this report ⇒
                // nothing to add; from another subtree ⇒ replayed, a fresh install as
                // AlreadyPresent so the caller records it once.
                Some(outcome) => {
                    if !dependencies.iter().any(|d| d.program == *dep) {
                        let result = match outcome {
                            DepResult::Installed { build, .. } => DepResult::AlreadyPresent(*build),
                            other => other.clone(),
                        };
                        dependencies.push(DepOutcome {
                            program: dep.clone(),
                            result,
                        });
                    }
                }
                // Still on the resolution stack (this program itself, or an ancestor).
                None => dependencies.push(DepOutcome {
                    program: dep.clone(),
                    result: DepResult::Skipped("requires cycle".into()),
                }),
            }
            continue;
        }
        if let Some(b) = active.get(dep).copied() {
            dependencies.push(DepOutcome {
                program: dep.clone(),
                result: DepResult::AlreadyPresent(b),
            });
            continue;
        }
        // Rule 2 on the requires path: a `system = "<bin>"` dependency the user's own
        // copy satisfies is NOT pulled in — the pass would only retire the managed copy
        // again next tick, and the user's copy is the one that runs anyway.
        let system = index
            .program(dep)
            .and_then(|p| p.system.as_deref())
            .and_then(|bin| {
                crate::vendor::system_binary_on_path(
                    &layout.prefix,
                    bin,
                    crate::elevate::path_var().as_deref(),
                )
            });
        if let Some(path) = system {
            dependencies.push(DepOutcome {
                program: dep.clone(),
                result: DepResult::System(path),
            });
            continue;
        }
        // An EXTRA is never opted in on a dependent's behalf (design S9): without this
        // machine's own opt-in marker it is SKIPPED with the consent spelling — a
        // store-managed parent installs without it, an OS-installed parent is stopped by
        // its requirement gate naming it — and no byte of it moves until the user says so.
        if index.is_extra(dep) && !layout.optin_exists(dep) {
            dependencies.push(DepOutcome {
                program: dep.clone(),
                result: DepResult::Skipped(crate::state::extra_not_installed(dep)),
            });
            continue;
        }
        let dep_req = InstallRequest {
            channel,
            program: dep.as_str(),
            triple,
            installed: None,
        };
        let (result, transitive) = match install_inner(
            fetcher,
            layout,
            anchor,
            &dep_req,
            floor,
            now_unix,
            seen,
            resolved_assets,
        ) {
            Ok(r) => (
                match r.protocol {
                    Some(outcome) => DepResult::Protocol(outcome),
                    None => DepResult::Installed {
                        build: r.build,
                        tree_root: r.tree_root,
                    },
                },
                r.dependencies,
            ),
            Err(e) => (DepResult::Skipped(e.to_string()), Vec::new()),
        };
        seen.insert(dep.clone(), Some(result.clone()));
        dependencies.push(DepOutcome {
            program: dep.clone(),
            result,
        });
        dependencies.extend(transitive); // flatten the transitive closure
    }

    // 5. The artifact for this triple (missing triple = clean fail-closed skip).
    let artifact = pkg
        .artifact_for(triple)
        .ok_or_else(|| FlowError::NoArtifact(triple.to_string()))?;
    // The pass's verified answer for this program, recorded BEFORE the download so a
    // transfer that fails midway still reaches the caller's collector — the pass-end
    // `gc::run_keeping_pinned_partials` spares exactly these programs' `<asset>.part`
    // resume files, and the failed download is the one the sparing exists for. (A pkg
    // row may name no `asset`; its local staging name is derived, see `pkg_local_name`.)
    resolved_assets.insert(program.to_string(), local_asset_name(program, artifact));
    // Per-member dispatch (§16.4/§17) on BOTH halves of the row: the tool path installs
    // plain `binary`/`cargo-src` (Shim — over `github-release` or `https`; only the
    // download lane differs, and `fetch_artifact` picks it from the PROTOCOL), a vendor
    // `.app` (VendorApp — the `https` + `app-bundle` + `dmg` lane, which lands in the
    // store and shims through its `links` exactly like Shim), AND `sysroot-bundle`
    // (trust / trust-mc) artifacts. A sysroot-bundle gets bundle-specific wiring
    // ([`apply_sysroot_bundle`]) BEFORE activation plus a fail-loud resolve check AFTER —
    // a failure past activation UNWINDS ([`abort_activated_install`]) so a broken
    // toolchain is neither reported SUCCESS nor left live reading as 'already current'.
    // `app-bundle` over `github-release` (the aterm self-update, applied in-session by the
    // app's own updater) keeps its own two-anchor gate and is NEVER the vendor-app lane.
    // The OS-INSTALLER lanes — `pkg` ([`apply_pkg`]), `softwareupdate`
    // ([`apply_softwareupdate`]) and `system-pm`
    // ([`apply_system_pm`]) — return HERE with a [`ProtocolOutcome`] and never reach the
    // store path below: nothing is staged, activated or shimmed for them. Unknown pairs
    // remain refused CLOSED. (audit: sysroot-bundle silent-broken-install;
    // sysroot-bundle resolve-failure left-active wedge.)
    //
    // Every row is admitted HERE, before the disk preflight and before any byte moves
    // (`vendor::check_row`, protocol-aware: the release lane has nothing to check, the
    // https lane keeps every refusal it always had, the OS-installer rows get their own).
    crate::vendor::check_row(artifact, &pkg.exposes)?;
    let strategy = crate::dispatch::strategy_for(&artifact.kind, &artifact.protocol);
    match strategy {
        crate::dispatch::ApplyStrategy::Shim
        | crate::dispatch::ApplyStrategy::SysrootBundle
        | crate::dispatch::ApplyStrategy::VendorApp => {}
        crate::dispatch::ApplyStrategy::AppBundle => {
            // Drive the two-anchor app-apply gate for the app's in-session apply topology and
            // fail closed (atpkg never swaps the app; see the helper for the deferral rule).
            return Err(app_apply_gate_refused(
                ch, program, pinned, artifact, installed,
            ));
        }
        crate::dispatch::ApplyStrategy::Pkg
        | crate::dispatch::ApplyStrategy::SoftwareUpdate
        | crate::dispatch::ApplyStrategy::SystemPm => {
            // 5b. The requirement gate for an OS-installed member: every `requires` must
            // be present FIRST. A dependency that DEFERRED (needs admin) defers this
            // member too — the explicit door installs both, in order — and one that
            // failed, or is UNAVAILABLE here (its manager absent), stops it here, with
            // the reason, rather than inside Apple's installer or the manager.
            for d in &dependencies {
                match &d.result {
                    DepResult::Protocol(o) if o.is_deferred() => {
                        let protocol = lane_name(strategy, artifact);
                        return Ok(protocol_report(
                            program,
                            pinned,
                            &index,
                            dependencies,
                            ProtocolOutcome::NeedsAdmin { protocol },
                        ));
                    }
                    DepResult::Protocol(o) if o.is_unavailable() => {
                        let why = o.state(&d.program);
                        return Err(FlowError::Requirement {
                            dep: d.program.clone(),
                            why,
                        });
                    }
                    // A dev-linked dependency is managed from its checkout, so it is met —
                    // exactly as `requires::unmet_requirement` counts it — though the
                    // recursion reports it as a `Linked` skip.
                    DepResult::Skipped(_) if crate::linkmode::is_linked(layout, &d.program) => {}
                    DepResult::Skipped(why) => {
                        return Err(FlowError::Requirement {
                            dep: d.program.clone(),
                            why: why.clone(),
                        });
                    }
                    DepResult::Installed { .. }
                    | DepResult::AlreadyPresent(_)
                    | DepResult::System(_)
                    | DepResult::Protocol(_) => {}
                }
            }
            let outcome = match strategy {
                crate::dispatch::ApplyStrategy::Pkg => {
                    apply_pkg(fetcher, layout, program, artifact)?
                }
                crate::dispatch::ApplyStrategy::SoftwareUpdate => {
                    apply_softwareupdate(layout, program, artifact)?
                }
                _ => {
                    let program_hint = index
                        .program(program)
                        .and_then(|p| p.unavailable_hint.as_deref());
                    apply_system_pm(layout, program, artifact, triple, program_hint)?
                }
            };
            return Ok(protocol_report(
                program,
                pinned,
                &index,
                dependencies,
                outcome,
            ));
        }
        crate::dispatch::ApplyStrategy::Unknown => {
            return Err(FlowError::UnsupportedKind(artifact.kind.clone()));
        }
    }

    // 6. Download → verify-and-stage (sha256 → extract → tree_root re-verify).
    //
    // 6a. THE DIGEST-REFUSAL MEMO first, because it is the one gate that can spare the
    // download entirely: a recent attempt at THIS build's signed digests already proved
    // the published asset does not match them, and nothing about that verdict changes
    // while the pin and its digests do not ([`digest_refusal_note`]). Consulted here —
    // after the index, the manifest and the row have all been verified, so a memo can
    // only ever skip a transfer, never admit bytes or choose a build.
    if let Some(note) = digest_refusal_note(layout, program, pinned, artifact) {
        return Err(FlowError::StageRefused(note));
    }
    let dl = staged_download_path(layout, program, &artifact.asset)?;
    // Disk preflight (§9): the compressed asset + its extracted tree must fit (they coexist
    // until the asset is reclaimed post-stage) while keeping the free floor. Fails OPEN when
    // free space can't be queried (available_bytes None) — preflight is a safety net.
    let required = artifact.size.saturating_add(artifact.cost.disk_installed);
    disk_gate(required, crate::freespace::available_bytes(&dl))?;
    if let Some(parent) = dl.parent() {
        std::fs::create_dir_all(parent).map_err(|e| FlowError::Download(e.to_string()))?;
    }
    // Clear any stale staging file BEFORE fetching — unless it is the verified archive
    // a previous attempt's failed stage RETAINED ([`archive_reusable`], 2026-09-15):
    // that one is the download, done, and is staged again without a fetch. A leftover
    // from a killed run is not merely junk: for a `dir:` registry it is a HARDLINK to
    // the source, and writing "into" it writes into the registry — `curl -o` truncates
    // it, and `fs::copy` truncates the shared inode. Both destroy a file inside the
    // user's signed app bundle. See `DirFetcher::download`; `archive_reusable` refuses
    // a hard link for exactly that reason.
    let reusable = carried_archive(&dl, artifact);
    if !reusable {
        let _ = std::fs::remove_file(&dl);
    }
    // A resumable transfer KEEPS its own `<asset>.part` across attempts — that is the
    // point — so the one thing that can leak here is a partial for an asset this program
    // has since moved past. `gc` reclaims those too (its `staging/` sweep removes every
    // regular file under `staging/<program>/` except a caller-named pinned partial, see
    // `gc::run_keeping_pinned_partials` — the sparing form every install/update pass ends
    // with, fed the `resolved_assets` this function collects; that wiring is what lets
    // THIS pinned partial outlive a failed pass at all), but `gc`'s standalone verb is one
    // a user may never run, so reclaim every OTHER partial now: at most one partial per
    // program survives between passes, and it is always the one the next attempt will
    // finish.
    sweep_foreign_partials(&dl);
    // Live-progress hooks (R5): the download runs in a curl child, so the byte meter
    // is a sibling `.part` poller against the SIGNED size. Every hook is a no-op
    // unless a `--progress-file` pass is live.
    crate::progress::note_build(program, pinned);
    // THE LANDING MARKER ([`crate::landing`], 2026-09-16): for an AGENT program whose
    // active build differs from `pinned`, `<prefix>/landing/<program>` stands from here —
    // inside the store lock, before the first byte moves — until this function returns,
    // whichever way (the guard's drop). Activation below re-lays `bin/` and the `agents/`
    // twin onto the new build before that drop, so a `claude` that handed over to
    // `atpkg __landing` on the marker runs the new build the moment `bin/` resolves into
    // it — and the verb reads the marker going WITHOUT that as the failed landing every
    // early `return Err` below is (the drop is the same on every exit; the verb never
    // takes "marker gone" for "landed").
    let landing_from =
        installed.or_else(|| crate::ops::active_builds(layout).get(program).copied());
    let landing_from_version = landing_from
        .filter(|&b| b != pinned && crate::stub::is_agent_program(program))
        .and_then(|b| version_of_build(fetcher, &index, &repo, program, b));
    let _landing = crate::landing::LandingGuard::begin(
        layout,
        program,
        landing_from,
        pinned,
        &pkg.version,
        landing_from_version.as_deref(),
    );
    let download_watch = crate::progress::watch_download(program, &dl, artifact.size);
    if reusable {
        println!(
            "atpkg: {program}: reusing the verified archive the previous attempt left \
             ({})",
            artifact.asset
        );
    } else if let Err(e) = fetch_artifact(fetcher, program, &repo, artifact, &dl) {
        // An aborted transfer leaves bytes in `<dl>.part`, not at `<dl>`: the production
        // fetcher promotes the part onto `dl` only on curl success, so `dl` is either
        // absent or complete. The part is deliberately LEFT for the next attempt to
        // continue from (`aterm_update_core::download_to_resumable`); `dl` itself is
        // reclaimed on this exit exactly as on the stage exit below, because a `dir:`
        // registry's copy lane writes there directly.
        let _ = std::fs::remove_file(&dl);
        return Err(FlowError::Download(e));
    }
    // The transfer is over either way — stop the poller before the phase moves on.
    drop(download_watch);
    crate::progress::note_phase(program, crate::progress::Phase::Verify);
    let build_dir = layout.build_dir(program, pinned);
    // Capture "this build is ALREADY live" BEFORE the stage swaps a new tree into it and
    // before `activate_channel` can move the links — by abort time the answer is gone. The
    // `installed` argument cannot answer it: it is the SHIM view and goes silent for a live
    // program whose tools were unlinked or tombstoned, which is the very reason `decide`
    // returned Install for a build that is already active. See `abort_activated_install`.
    let was_live = std::fs::read_link(layout.program_current(program))
        .is_ok_and(|t| t == build_dir)
        || std::fs::read_link(layout.channel_current(channel)).is_ok_and(|t| t == build_dir);
    // Preflight again before extract: the asset is already downloaded, so only the extracted
    // tree remains to fit. Reclaim the asset before returning — the ONE failure whose
    // meaning is "the volume is full" must not walk away leaving the thing making it fuller.
    if let Err(e) = disk_gate(
        artifact.cost.disk_installed,
        crate::freespace::available_bytes(&build_dir),
    ) {
        let _ = std::fs::remove_file(&dl);
        return Err(e);
    }
    // Reclaim the compressed asset on EVERY exit, not just the happy one: a stage that fails
    // (bad mirror, tree_root mismatch, full disk) otherwise strands a full archive in
    // `staging/` forever — nothing else ever sweeps that directory, since
    // `gc::interrupted_debris` walks `store/` only — and a member that keeps failing keeps
    // leaking, one copy per distinct asset name.
    // The verify→extract boundary lives inside `verify_and_stage`; the extract scope
    // credits `write_capped`'s loop to this program, and the first written byte flips
    // the phase label honestly (see `progress::extract_scope`).
    // The shim environment the signed manifest declares (design S7) is recorded BESIDE
    // the build (`<build>.shim-env`) before the tree is staged: the verbs that hold no
    // manifest — the transaction's rollback, `rollback`, `unlink`'s restore — re-lay this
    // build's shims from it. Before the stage, so a build the sidecar could not be
    // written for is never marked ready with shims that would lack their environment.
    if let Err(e) = crate::shim_env::write_sidecar(&build_dir, &pkg.shim_env()) {
        let _ = std::fs::remove_file(&dl);
        let mut why = String::from("shim_env sidecar: ");
        why.push_str(&e.to_string());
        return Err(FlowError::Activate(why));
    }
    let extract_scope = crate::progress::extract_scope(program, artifact.cost.disk_installed);
    let staged = verify_and_stage(artifact, &dl, &build_dir);
    drop(extract_scope);
    if let Err(e) = staged {
        record_digest_refusal(&build_dir, artifact, &e);
        reclaim_after_failed_stage(&dl, &e);
        return Err(FlowError::Stage(e));
    }
    let _ = std::fs::remove_file(&dl);
    // This build's bytes are good, whatever an earlier pass recorded about them — a
    // publisher who repaired the asset under the same pin is exactly the case the
    // cooldown exists to let through, so the memo goes with the verdict it held.
    crate::store::clear_stage_refusal(&build_dir);

    // 6b. Sysroot-bundle wiring BEFORE activation (self-contained = no-op).
    if strategy == crate::dispatch::ApplyStrategy::SysrootBundle {
        apply_sysroot_bundle(&artifact.reloc)?;
    }

    // 7. Activate + shim. The raw manifest `exposes` is admitted ONCE here; `tools` is what
    // actually got a shim and `refused` the sensitive/malformed names that did not.
    crate::progress::note_phase(program, crate::progress::Phase::Link);
    activate_channel(layout, channel, &build_dir)
        .map_err(|e| FlowError::Activate(e.to_string()))?;
    let (tools, refused) = crate::store::split_exposed(&pkg.exposes);
    // Past activation a failure leaves the broken build LIVE — channel `current`,
    // per-program witness, any shims already written — AND carrying its `.ready`
    // marker, so `active_builds` reports it, `decide` calls it UpToDate, and a retry
    // prints 'already current' with nothing working (the wedge `flip_member` rolls
    // back on the transactional path). Capture the same rollback input here so both
    // error arms below unwind identically. (audit: resolve-failure left-active wedge.)
    // ALab's own tools get their `alab-<tool>` aliases; a vendor extra or a
    // system-satisfiable member does not (`activate.rs` module doc).
    let aliases = Aliases::for_program(program, index.program(program));
    let staged = Staged {
        build: pinned,
        build_dir: build_dir.clone(),
        exposes: tools.clone(),
        prior_build: installed,
        was_live,
        reloc: None,
        tree_root: String::new(),
        aliases,
    };
    // The shims export the signed manifest's `shim_env` (design S7) — a managed vendor
    // tool runs with its own updater off; a system copy never runs through a shim.
    if let Err(e) = install_tools_env(layout, &build_dir, &tools, aliases, &pkg.shim_env()) {
        abort_activated_install(layout, channel, program, &staged);
        return Err(FlowError::Activate(e.to_string()));
    }

    // 7b. Fail-loud resolve check: an installed sysroot-bundle's compilers must
    // actually load. A dynamic-loader failure here aborts — and UNWINDS — the install.
    if strategy == crate::dispatch::ApplyStrategy::SysrootBundle
        && let Err(e) = bundle_resolve_check(&build_dir, &tools)
    {
        abort_activated_install(layout, channel, program, &staged);
        return Err(e);
    }
    // NOTE: the shell.d hook refresh runs at the main.rs CLI edge (do_install / cmd_update),
    // NOT here — writing ~/.aterm from flow's synthetic-layout unit tests would pollute the
    // developer's real home (identical hermeticity reasoning as the GC-after-activate edge).
    let shimmed: Vec<String> = tools.iter().map(|t| t.as_str().to_string()).collect();
    Ok(InstallReport {
        program: program.to_string(),
        build: pinned,
        index_build: index.index_build,
        roster_seq: index.roster_seq(),
        already_current: false,
        shimmed,
        refused_shims: refused,
        tree_root: artifact.tree_root.clone(),
        dependencies,
        protocol: None,
    })
}

/// The report an OS-installer lane returns: the pinned build (the index's word for
/// "which row"), no shims, no root, and the lane's [`ProtocolOutcome`].
fn protocol_report(
    program: &str,
    pinned: u64,
    index: &TrustedIndex,
    dependencies: Vec<DepOutcome>,
    outcome: ProtocolOutcome,
) -> InstallReport {
    InstallReport {
        program: program.to_string(),
        build: pinned,
        index_build: index.index_build,
        roster_seq: index.roster_seq(),
        already_current: false,
        shimmed: vec![],
        refused_shims: vec![],
        tree_root: String::new(),
        dependencies,
        protocol: Some(outcome),
    }
}

/// The spelling an OS-installer lane's state prints for `artifact`: the protocol for
/// `pkg` and `softwareupdate`, the MANAGER's name for a `system-pm` row (`apt`, …; the
/// protocol's own name only for a manager the table does not carry, which admission
/// refuses before this is ever read).
fn lane_name(
    strategy: crate::dispatch::ApplyStrategy,
    artifact: &crate::manifest::Artifact,
) -> &'static str {
    match strategy {
        crate::dispatch::ApplyStrategy::SoftwareUpdate => crate::softwareupdate::PROTOCOL,
        crate::dispatch::ApplyStrategy::SystemPm => {
            crate::vendor::manager(&artifact.manager).map_or(crate::system_pm::PROTOCOL, |m| m.name)
        }
        _ => crate::installer_pkg::PROTOCOL,
    }
}

/// The `pkg` lane's flow half: PROBE first (a `provides` path that exists is the whole
/// answer — nothing is ever re-downloaded for a member that is present), DEFER when this
/// thread may not elevate (the unattended pass, a door with no terminal — no byte moves
/// for a member that cannot be applied), else DOWNLOAD through the https lane under the
/// signed `size` cap and `sha256`, hand the file to [`crate::installer_pkg::install`]
/// (signature team, elevated installer, provides), and RECLAIM the file on every path.
fn apply_pkg(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    program: &str,
    artifact: &crate::manifest::Artifact,
) -> Result<ProtocolOutcome, FlowError> {
    let protocol = crate::installer_pkg::PROTOCOL;
    let path_var = crate::elevate::path_var();
    if let Some(path) =
        crate::elevate::first_provided(&layout.prefix, &artifact.provides, path_var.as_deref())
    {
        return Ok(ProtocolOutcome::Installed { protocol, path });
    }
    let elevation = crate::elevate::elevation();
    if elevation == crate::elevate::Elevation::Deferred {
        return Ok(ProtocolOutcome::NeedsAdmin { protocol });
    }
    let dl = download_pkg(fetcher, layout, program, artifact)?;
    let applied = crate::elevate::with_current_runner(|runner| {
        crate::installer_pkg::install(
            runner,
            elevation,
            artifact,
            &dl,
            &layout.prefix,
            path_var.as_deref(),
        )
    });
    // The package is spent either way: installed, or refused/failed and re-downloaded
    // on the next explicit attempt (its `.part` was promoted on success, so nothing
    // resumable is left behind either).
    let _ = std::fs::remove_file(&dl);
    let path = applied.map_err(|why| FlowError::Protocol { protocol, why })?;
    Ok(ProtocolOutcome::Installed { protocol, path })
}

/// The `softwareupdate` lane's flow half: probe, defer, else run
/// [`crate::softwareupdate::install`] with the real placeholder. No bytes of ours move.
fn apply_softwareupdate(
    layout: &Layout,
    program: &str,
    artifact: &crate::manifest::Artifact,
) -> Result<ProtocolOutcome, FlowError> {
    let protocol = crate::softwareupdate::PROTOCOL;
    let path_var = crate::elevate::path_var();
    if let Some(path) =
        crate::elevate::first_provided(&layout.prefix, &artifact.provides, path_var.as_deref())
    {
        return Ok(ProtocolOutcome::Installed { protocol, path });
    }
    let elevation = crate::elevate::elevation();
    if elevation == crate::elevate::Elevation::Deferred {
        return Ok(ProtocolOutcome::NeedsAdmin { protocol });
    }
    crate::progress::note_phase(program, crate::progress::Phase::Link);
    let path = crate::elevate::with_current_runner(|runner| {
        crate::softwareupdate::install(
            runner,
            elevation,
            artifact,
            Path::new(crate::softwareupdate::PLACEHOLDER),
            &layout.prefix,
            path_var.as_deref(),
        )
    })
    .map_err(|why| FlowError::Protocol { protocol, why })?;
    Ok(ProtocolOutcome::Installed { protocol, path })
}

/// The `system-pm` lane's flow half ([`crate::system_pm`]): PROBE `provides` first (one
/// exists ⇒ `installed via <manager>: <path>`, nothing runs, whatever the policy); the
/// MANAGER next — absent from `PATH` ⇒ [`ProtocolOutcome::Unavailable`] (`unavailable
/// on <target>: <hint>`; atpkg never installs a manager, so nothing is deferred to the
/// door either); DEFER when the row declares `elevated = true` and this thread may not
/// elevate (`needs admin`; a user-scoped row runs unattended — it needs no one's
/// password); else RUN the manager through [`crate::system_pm::install`] under the
/// current runner and prove the install. `program_hint` is the index's
/// `[programs.<name>].unavailable_hint`, folded into the missing-manager hint.
fn apply_system_pm(
    layout: &Layout,
    program: &str,
    artifact: &crate::manifest::Artifact,
    triple: &str,
    program_hint: Option<&str>,
) -> Result<ProtocolOutcome, FlowError> {
    let Some(mgr) = crate::vendor::manager(&artifact.manager) else {
        // Unreachable past `check_row`, which refuses an unknown manager by name; kept
        // as a refusal rather than a panic so a future admission slip fails closed.
        let mut why = String::from("manager is not in the table: ");
        why.push_str(&artifact.manager);
        return Err(FlowError::Protocol {
            protocol: crate::system_pm::PROTOCOL,
            why,
        });
    };
    let protocol = mgr.name;
    let path_var = crate::elevate::path_var();
    if let Some(path) =
        crate::elevate::first_provided(&layout.prefix, &artifact.provides, path_var.as_deref())
    {
        return Ok(ProtocolOutcome::Installed { protocol, path });
    }
    let Some(manager_bin) =
        crate::system_pm::manager_on_path(&layout.prefix, mgr, path_var.as_deref())
    else {
        return Ok(ProtocolOutcome::Unavailable {
            protocol,
            target: triple.to_string(),
            hint: crate::system_pm::missing_manager_hint(mgr, &artifact.package, program_hint),
        });
    };
    let elevation = crate::elevate::elevation();
    if artifact.elevated && elevation == crate::elevate::Elevation::Deferred {
        return Ok(ProtocolOutcome::NeedsAdmin { protocol });
    }
    crate::progress::note_phase(program, crate::progress::Phase::Link);
    let path = crate::elevate::with_current_runner(|runner| {
        crate::system_pm::install(
            runner,
            elevation,
            mgr,
            artifact,
            &manager_bin,
            &layout.prefix,
            path_var.as_deref(),
        )
    })
    .map_err(|why| FlowError::Protocol { protocol, why })?;
    Ok(ProtocolOutcome::Installed { protocol, path })
}

/// Download a `pkg` row's package into `staging/<program>/<local name>` through the
/// https lane (signed `url`, signed `size` as the cap) and gate it on the signed
/// `sha256` — the same integrity gate every store member passes, applied to a file the
/// store never keeps. Same partial-resume discipline as the store path.
fn download_pkg(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    program: &str,
    artifact: &crate::manifest::Artifact,
) -> Result<PathBuf, FlowError> {
    let dl = layout
        .staging_dir(program)
        .join(local_asset_name(program, artifact));
    disk_gate(
        artifact.size.saturating_add(artifact.cost.disk_installed),
        crate::freespace::available_bytes(&dl),
    )?;
    if let Some(parent) = dl.parent() {
        std::fs::create_dir_all(parent).map_err(|e| FlowError::Download(e.to_string()))?;
    }
    let _ = std::fs::remove_file(&dl);
    sweep_foreign_partials(&dl);
    let download_watch = crate::progress::watch_download(program, &dl, artifact.size);
    if let Err(e) = fetch_artifact(fetcher, program, "", artifact, &dl) {
        let _ = std::fs::remove_file(&dl);
        return Err(FlowError::Download(e));
    }
    drop(download_watch);
    crate::progress::note_phase(program, crate::progress::Phase::Verify);
    let got = match crate::tree::file_sha256(&dl) {
        Ok(h) => h,
        Err(e) => {
            let _ = std::fs::remove_file(&dl);
            return Err(FlowError::Stage(StageError::Io(e)));
        }
    };
    if !got.eq_ignore_ascii_case(&artifact.sha256) {
        let _ = std::fs::remove_file(&dl);
        discard_sibling_partial(&dl);
        return Err(FlowError::Stage(StageError::Sha256Mismatch {
            expected: artifact.sha256.clone(),
            got,
        }));
    }
    Ok(dl)
}

/// The local staging file name for a row: its `asset` (every store-bound row carries
/// one), or for a `pkg` row that omits it the URL's last path component when that is a
/// bare name, else `<program>.pkg`. Only ever joined onto `staging/<program>/`.
fn local_asset_name(program: &str, artifact: &crate::manifest::Artifact) -> String {
    if !artifact.asset.is_empty() || artifact.protocol != "pkg" {
        return artifact.asset.clone();
    }
    let tail = artifact
        .url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("");
    if !tail.is_empty() && tail != "." && tail != ".." && !tail.contains('\\') {
        return tail.to_string();
    }
    let mut n = String::from(program);
    n.push_str(".pkg");
    n
}

/// The ONE place the download lanes fork — on the row's PROTOCOL, never its kind or its
/// apply strategy. A `github-release` member goes through [`Fetcher::download_for`] (the
/// program's `repo` under the account slug, the `[packages.links]` override, the GitHub
/// token) — with the SIGNED `size` as its byte cap, the same number the disk preflight
/// bounded the free-space check with, so no lane may write more than the preflight
/// admitted. An `https` member — and a `pkg` member, whose package is a vendor download
/// like any other — goes through [`Fetcher::download_url`] with the SIGNED row's `url`
/// and the SIGNED `size` as the exact byte cap — and NEVER through `download_for`: the
/// vendor host is not a release repo, and nothing about the account slug may reach it.
/// `system-pm` and `softwareupdate` move no bytes of ours (their tools fetch their own);
/// anything else is refused by name. The row was admitted by
/// [`crate::vendor::check_row`] before this is called.
fn fetch_artifact(
    fetcher: &dyn Fetcher,
    program: &str,
    repo: &str,
    artifact: &crate::manifest::Artifact,
    dl: &Path,
) -> Result<(), String> {
    match artifact.protocol.as_str() {
        "github-release" => fetcher.download_for(program, repo, &artifact.asset, dl, artifact.size),
        "https" | "pkg" => fetcher.download_url(&artifact.url, dl, artifact.size),
        other => {
            let mut m = String::from("protocol ");
            m.push_str(other);
            m.push_str(" has no download lane in this client");
            Err(m)
        }
    }
}

/// Install a failing tombstone shim (§7) over EVERY tool `program`'s currently-active build
/// exposes, so a yanked/below-floor build's old working shims are actively disabled. The tool
/// set is the program's live `bin/` shims (the exact "old working shims" to revoke); if the
/// program is not installed there is nothing to disable. Best-effort per tool (a write failure
/// on one shim never blocks the rest); a tombstone can never shadow a sensitive name because
/// `active_tools` hands back [`crate::store::ToolName`]s, which only exist for admitted names.
fn install_tombstone_shims(layout: &Layout, program: &str, installed: Option<u64>) {
    if let Some(cur) = installed {
        // The `alab-<tool>` aliases forward to the same revoked executable, so they are
        // disabled with their primaries — a revoked build must not stay runnable under
        // its other name.
        let tools = crate::ops::active_tools(layout, program, cur);
        let aliases = crate::ops::active_aliases(layout, program, cur);
        for tool in tools.iter().chain(&aliases) {
            let _ = install_tombstone_shim(layout, tool);
        }
    }
}

/// Unwind a per-program install that failed strictly AFTER [`activate_channel`]
/// (`install_tools` / [`bundle_resolve_check`]). Three steps, each already proven
/// elsewhere: [`rollback_member`] restores the prior build's whole shim surface +
/// links (an upgrade reverts; a fresh install removes its shims + witness);
/// [`crate::activate::undo_activation`] then sweeps any pointer STILL naming the
/// doomed build — the channel `current` a fresh-install rollback has no prior to
/// re-point at — and is a no-op after a successful prior-build restore (it only
/// removes links/shims resolving INTO `build_dir`); finally the staged build is
/// DISCARDED so `active_builds`/`decide` can never re-read the broken tree as
/// complete and answer 'already current' on a retry.
///
/// …EXCEPT when `staged.was_live`, i.e. this install re-staged the build that was ALREADY
/// live. The discard's whole justification is "do not leave behind a build a retry will
/// trust", and it is sound for a build this call created. It is not sound for one that was
/// live and complete before the call: the failures that land here are `install_tools`
/// (writing `bin/` shims — an EACCES, EROFS or full disk, i.e. a fact about the environment
/// and not about the tree) and the resolve check. Deleting a multi-gigabyte verified tree in
/// response to a failed shim write leaves the user with NO toolchain, and re-downloading it
/// is exactly what the failing condition tends to forbid. `decide` cannot be fooled by what
/// survives, either: `undo_activation` still runs, so no shim resolves into the tree and
/// `active_builds` stays silent about it, and the next run RE-STAGES this build in place
/// rather than answering 'already current'. Reached whenever the SHIM-derived `installed`
/// view is silent for a live program (`atpkg unlink`, tombstoned tools, dev-link mode) and
/// `decide` therefore returns Install for the build `store/<program>/current` already
/// names — the same blind spot [`Staged::was_live`] exists for in the group transaction.
///
/// **AND THE KEEP HAS TO OUTLIVE THE RUN**, which it did not while this arm merely returned.
/// [`Staged::was_live`] is computed (step 6) from exactly two links, and the two steps above
/// remove BOTH for this member: the shim view being silent is what makes `prior_build`
/// `None`, so `rollback_member`'s fresh-install arm takes `store/<program>/current`, and
/// `undo_activation` takes the channel link. The retry therefore probed `was_live == false`,
/// and the SECOND failure of the same unwritable `bin/` fell through to the discard and
/// deleted the multi-gigabyte verified tree the FIRST abort had deliberately kept — the
/// toolchain gone on the one machine least able to re-download it, and [`crate::gc`]
/// abstaining in the meantime only because the witness was missing rather than because
/// anything claimed the tree. So the per-program witness is re-pointed at the kept build:
/// the state `was_live` just PROVED the store was in before this run, which makes the keep
/// hold for every retry and makes `gc::live_builds` call the tree this program's live build
/// instead of an unwitnessed one. Only when nothing else claims that link — a rollback to a
/// DIFFERENT prior build re-pointed it already, and that restore wins — and only for a tree
/// whose `.ready` marker still reads complete. The CHANNEL link is deliberately not
/// restored: it is one link per channel, shared by every program, it need not have named
/// this build before the run, and it is not what the next `was_live` probe or gc's
/// per-program authority reads.
fn abort_activated_install(layout: &Layout, channel: &str, program: &str, staged: &Staged) {
    rollback_member(layout, channel, program, staged);
    crate::activate::undo_activation(layout, channel, &staged.build_dir);
    if staged.was_live {
        // Re-point the witness at the tree this arm keeps, or the keep is one-shot: the
        // two steps above removed the very links the next run's `was_live` reads. EXISTS,
        // not resolves, is the claim test — the same one `gc::authority_claims` applies —
        // so a link a prior-build rollback already wrote is never overwritten here.
        if crate::store::build_is_complete(&staged.build_dir)
            && std::fs::symlink_metadata(layout.program_current(program)).is_err()
        {
            let _ = crate::activate::atomic_symlink(
                &staged.build_dir,
                &layout.program_current(program),
            );
        }
        return;
    }
    crate::store::discard_build(&staged.build_dir);
}

/// Drive the two-anchor app-apply gate ([`crate::appgate::app_apply_allowed`], §16.2/§16.4)
/// for an `app-bundle` member met on the tool-install path, and return the fail-closed refusal.
///
/// `aterm.app` is a NOTARIZED DMG the running app applies to itself IN-SESSION (aterm-gui's
/// overlap handoff: a successor is spawned and every PTY is handed across, so the shells keep
/// running; the cold-launch swap in aterm-update is only the fallback) — a distinct topology
/// from the `bin/` symlink flip. The atpkg CLI tool-install path does not carry it, so Apple
/// **notarization is unproven here** — and because notarization is the gate's UNCONDITIONAL
/// AND-anchor, the decision fails closed regardless of the index conjunct. We still evaluate
/// the REAL gate (with the fresh-index conjunct built from the signed channel `min_build` +
/// per-build yank state) so the refusal is the gate's decision, not a blanket reject.
///
/// NOTE(app-bundle): atpkg must never swap, re-exec, or ask the user to reopen the app. If the
/// `aterm` member is ever added to the index, atpkg's whole job is refuse-and-defer plus a
/// stage handoff to the app's own in-session lane (also recorded in
/// docs/TOOLCHAIN-PACKAGE-MANAGER.md §16.4):
///   (a) verify the DMG through [`crate::appgate::app_apply_allowed`] with a REAL notarization
///       result — call aterm-update's `verify_bundle`, never reimplement codesign/spctl;
///   (b) stage by calling aterm-update's own publish routine so the bundle lands at
///       `aterm_update::paths::Staging::staged_app` (`<updates_root>/staged/aterm.app`) with
///       `ready.toml` written last under `stage.lock`;
///   (c) release the seal-read marker (`Updates/toolchain-install`, [`crate::net`]'s
///       `SealReadGuard`) before returning — while it is held the GUI's apply is vetoed by
///       `aterm_update::is_toolchain_install_active()`, so an app stage must not go through
///       a DirFetcher on the sealed seed;
///   (d) poke the running GUI with `aterm ctl update check` (Owner-only), whose `check` arm
///       must post `Wake::UpdateStaged` for a strictly-newer stage so the App's reconcile
///       arms automatic apply after validating the durable stage;
///   (e) the only user-visible sentence is "staged for aterm to apply in-session" — never
///       spawn, exec, or prompt.
fn app_apply_gate_refused(
    ch: &Channel,
    program: &str,
    pinned: u64,
    _artifact: &crate::manifest::Artifact,
    installed: Option<u64>,
) -> FlowError {
    let gate = crate::appgate::AppIndexGate {
        // No staged DMG to hash on this path, so the sha256 conjunct cannot be satisfied (see
        // the NOTE above); the notarization anchor already fails the gate closed.
        sha256_match: false,
        min_build: ch.min_build_for(program),
        yanked: crate::gate::is_yanked(ch, program, pinned),
    };
    // notarized = false: the CLI install path proves no Apple notarization and never applies
    // the app itself, so the unconditional AND-anchor refuses the apply.
    let allowed =
        crate::appgate::app_apply_allowed(false, pinned, installed.unwrap_or(0), Some(&gate));
    debug_assert!(
        !allowed,
        "notarization is unproven on the CLI path — the gate must fail closed"
    );
    FlowError::AppBundleRefused(program.to_string())
}

/// A member staged (verified + extracted) but NOT yet flipped, plus what `flip`/`rollback`
/// need: its new build dir, exposed binaries, and the build its shims pointed at BEFORE
/// (for the rollback swap-back). Collected during the stage phase of [`transact`].
struct Staged {
    build: u64,
    build_dir: PathBuf,
    /// The tools that will actually be shimmed — the ADMITTED set, not the raw manifest list.
    /// Holding [`ToolName`]s here rather than `Vec<String>` forces `rollback_member` to probe
    /// the prior build via `exe_file()`, never the bare name — see [`ToolName`]'s docs.
    exposes: Vec<ToolName>,
    /// The member's prior active build (`None` ⇒ a fresh install: rollback removes the shims).
    prior_build: Option<u64>,
    /// `true` when this member's `build_dir` was ALREADY the live build when we staged it: a
    /// `current` authority link named it while the SHIM-derived `installed` view
    /// ([`crate::ops::active_builds`]) was silent (every tool tombstoned, or `atpkg unlink`
    /// removed the dev shims), so `decide` returned Install for the build that is already
    /// active. The abort discard must never delete such a tree.
    was_live: bool,
    /// `Some(reloc_policy)` for a `sysroot-bundle` member (its pre-activation wiring +
    /// post-activation resolve check run during the flip); `None` for a plain Shim.
    reloc: Option<String>,
    /// The SIGNED `tree_root` of the staged build, surfaced in [`ChannelApplyReport::applied`]
    /// so the CLI can record it for `atpkg verify` after an update flip.
    tree_root: String,
    /// Whether the member's tools get their `alab-<tool>` aliases — read off the signed
    /// index entry ([`Aliases::for_program`]) when it is staged, or off the disk
    /// ([`Aliases::laid_for`]) by the rollback verb, which holds no index. The flip lays
    /// them with the primaries and the rollback re-points or removes them with the
    /// primaries.
    aliases: Aliases,
}

/// The result of a whole-channel transactional update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelApplyReport {
    /// The `index_build` of the signed index this apply trusted (to advance the floor).
    pub index_build: u64,
    /// The `roster_seq` that authorized it (to advance the durable roster floor).
    pub roster_seq: u64,
    /// Per coherence group: the group and its transaction outcome.
    pub groups: Vec<(Group, TxnOutcome)>,
    /// Per member the apply flipped LIVE: its new build + SIGNED `tree_root`, so the CLI can
    /// persist the root into `status.toml` for `atpkg verify`. Keyed by program name.
    pub applied: BTreeMap<String, AppliedMember>,
    /// Members excluded from this apply because they are dev-linked (§13) — reported so the
    /// CLI can note the skip.
    pub skipped_linked: Vec<String>,
    /// `program → pinned artifact asset file name` for every member whose release-verified
    /// manifest this pass resolved far enough to SELECT an artifact — INCLUDING members
    /// whose download or stage then failed, which is the point: the pass-end
    /// [`crate::gc::run_keeping_pinned_partials`] spares exactly these programs'
    /// `<asset>.part` resume files, and the failed member is the one with a partial worth
    /// sparing. A program absent here gets nothing spared (the safe default — the sparing
    /// closure only ever SHRINKS the sweep, so an empty map is plain `gc::run`).
    pub resolved_assets: BTreeMap<String, String>,
}

/// A member the apply flipped live: its new build + the SIGNED `tree_root` for `atpkg verify`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMember {
    /// The build now active.
    pub build: u64,
    /// The signed `tree_root` of that build.
    pub tree_root: String,
}

/// What a [`rollback`] did: the program moved from `from_build` down to `to_build`, resolved
/// against the signed index at `index_build`. `coherence_group` (if any) lets the CLI WARN
/// that rolling back one member of a locked tuple splits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackReport {
    /// The program rolled back.
    pub program: String,
    /// The build it was active on before.
    pub from_build: u64,
    /// The gate-valid build it is now active on (strictly below `from_build`).
    pub to_build: u64,
    /// The `index_build` of the signed index this rollback trusted (advance the floor to it).
    pub index_build: u64,
    /// The `roster_seq` that authorized it (advance the durable roster floor to it).
    pub roster_seq: u64,
    /// The program's coherence group, if it is in one (a per-program rollback splits it).
    pub coherence_group: Option<String>,
}

/// Update EVERY program the channel pins, **group by group**, each coherence group applied
/// as an all-or-nothing transaction (§7): the `rustc`-locked tuple (`trust`/`ay`/…) moves
/// atomically — stage every member, and only once ALL staged, flip every member; a stage
/// failure flips nothing, a flip failure rolls the already-flipped members back — while an
/// ungrouped tool applies independently so it can never wedge the tuple.
///
/// The index is resolved + verified ONCE (unlike the per-program [`install`] loop, which
/// re-verified it each call and could not move a group atomically). `installed` maps each
/// program to its currently-active build. Never re-fetches the index between members, so a
/// mid-run index change can't split a group across two index states.
///
/// ONCE *PER CALL*, which is not once per PASS: a caller that runs a second lane after
/// this one (`atpkg update`'s attestation repair and set completion) must hand the index
/// it already holds to [`apply_channel_with`] rather than call this and resolve again.
#[allow(
    clippy::too_many_arguments,
    reason = "every input is an irreducible dependency of a verified channel apply: the \
              network fetcher, the install layout, the pinned root key, the channel + \
              triple selectors, the installed-build map, and the floor + clock the anti-\
              rollback and freshness gates read — bundling them into a struct would only \
              move the count to the single call site"
)]
pub fn apply_channel(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    channel: &str,
    triple: &str,
    installed: &BTreeMap<String, u64>,
    excluded: &[String],
    floor: BuildFloor,
    now_unix: i64,
) -> Result<ChannelApplyReport, FlowError> {
    // 1–2. Resolve + verify-select the index ONCE + freshness (§8) — the shared
    //      [`resolve_verified_index`] prologue (cached-fallback, §14).
    let index = resolve_verified_index(fetcher, layout, anchor, floor, now_unix)?;
    apply_channel_with(
        fetcher, layout, &index, channel, triple, installed, excluded,
    )
}

/// [`apply_channel`] over a CALLER-RESOLVED, already-verified index — the shape
/// [`bootstrap_group`] has always had, for a caller whose pass has more than one lane.
///
/// `atpkg update` is that caller. One pass applies the channel, repairs a missing
/// attestation and then completes the default set, and every lane used to resolve the
/// index for itself: the same `index-cache.toml` re-read and TOML-parsed, the same
/// master-signature verify + roster admit + machine-signature verify over every
/// candidate, the same roster-generation observation and its floor write — two to four
/// times per six-hourly tick, for an answer that cannot differ inside one pass (the
/// production fetcher memoizes its listing, so the repeats were never even network
/// work). Resolving once and threading the result is also the STRONGER property: a pass
/// that re-resolves could in principle read two index states, which is the split §7
/// exists to forbid.
///
/// The gates are the caller's, exactly as they are for [`bootstrap_group`]: this
/// function verifies nothing itself, so `index` must come from
/// [`resolve_verified_index`] (anti-rollback floor, roster admission, machine signature,
/// freshness), and a caller holding one across a long pass re-checks its freshness window
/// before handing it to the next lane.
pub fn apply_channel_with(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    channel: &str,
    triple: &str,
    installed: &BTreeMap<String, u64>,
    excluded: &[String],
) -> Result<ChannelApplyReport, FlowError> {
    // The channel as THIS target sees it (`pin_by_target` laid over `pin`): every decide,
    // plan and fetch below reads this view, never the raw platform-agnostic pin.
    let ch = index
        .channel_for(channel, triple)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;

    // 3. Partition the channel's pins into coherence groups (grouped tuples + singletons)
    //    and apply each as its own all-or-nothing transaction.
    let groups = plan_groups(index, &ch);
    let mut results = Vec::with_capacity(groups.len());
    let mut applied: BTreeMap<String, AppliedMember> = BTreeMap::new();
    let mut skipped_linked: Vec<String> = Vec::new();
    let mut resolved_assets: BTreeMap<String, String> = BTreeMap::new();
    for group in &groups {
        // Dev-linked HARD-SKIP (§13): a coherence tuple with ANY linked member is skipped
        // whole (can't partial-move a locked group over a dev link).
        if group
            .members
            .iter()
            .any(|m| crate::linkmode::is_linked(layout, m))
        {
            for m in &group.members {
                if crate::linkmode::is_linked(layout, m) {
                    skipped_linked.push(m.clone());
                }
            }
            continue;
        }
        if let Some((acted, outcome, group_applied)) = apply_group(
            fetcher,
            layout,
            index,
            &ch,
            channel,
            triple,
            group,
            installed,
            excluded,
            &mut resolved_assets,
        ) {
            applied.extend(group_applied);
            results.push((acted, outcome));
        }
    }
    // (Shell.d hook refresh runs at the main.rs CLI edge, not here — see the note in
    // `install` — to keep apply_channel's unit tests hermetic w.r.t. the real ~/.aterm.)
    Ok(ChannelApplyReport {
        index_build: index.index_build,
        roster_seq: index.roster_seq(),
        groups: results,
        applied,
        skipped_linked,
        resolved_assets,
    })
}

/// Is any member of `group` INSTALLED HERE BUT DISABLED — a build this prefix still
/// witnesses as live, whose `bin/` shims were replaced by the failing TOMBSTONE scripts a
/// revocation writes (§7)?
///
/// The update lane's group selection reads the shim view, which is deliberately SILENT for
/// a tombstone ([`crate::platform::resolve_shim`] yields `None` for a script with no `exec`
/// line). That silence is the self-heal for a member of a group that still has an installed
/// sibling, and a dead end for a group that has none — the case this answers.
///
/// Both halves are required, and each is narrow on purpose:
///
/// * **A shim that is present and refuses to run.** Not a MISSING shim: that is an
///   interrupted uninstall or a hand-removed file, and treating it as "installed" would let
///   this lane reinstall a program nobody asked it to (the update pass must never install a
///   missing dependency — it holds the dependent instead).
/// * **A witness for the program's live build, whose `bin/` holds that tool's executable.**
///   A tombstone carries no target, so the only honest way back to its program is the build
///   [`crate::gc::live_builds`] proves live: a non-resolving `bin/<tool>` whose name matches
///   an executable of THAT build is this program's disabled command. Without the match, any
///   tombstone anywhere would speak for every program in the prefix.
///
/// One `live_builds` scan and one `bin/` scan per call, on the path that was otherwise
/// about to skip the group.
fn any_member_tombstoned_in_place(layout: &Layout, group: &Group) -> bool {
    let Ok(entries) = std::fs::read_dir(layout.bin_dir()) else {
        return false;
    };
    // Every `bin/` name that is a shim shape this manager could have written AND currently
    // forwards nowhere — the tombstones, plus anything else inert that got the same name.
    let tombstones: Vec<ToolName> = entries
        .flatten()
        .filter(|e| crate::platform::resolve_shim(&e.path()).is_none())
        .filter_map(|e| ToolName::from_shim_file(e.file_name().to_str()?))
        .collect();
    if tombstones.is_empty() {
        return false;
    }
    let live = crate::gc::live_builds(layout);
    group.members.iter().any(|m| {
        live.get(m).is_some_and(|witness| {
            let build_bin = layout.build_dir(m, witness.build()).join("bin");
            // `presence`, not `exists`: an EACCES/EPERM on the build dir is not proof the
            // tool is gone, and the fail-closed direction for a REVIVAL is to keep looking.
            tombstones
                .iter()
                .any(|tool| !crate::store::presence(&build_bin.join(tool.exe_file())).is_absent())
        })
    })
}

/// Apply ONE coherence group as an all-or-nothing transaction (the per-group body factored
/// out of [`apply_channel`], shared with the transactional [`apply_program`] update path).
/// `None` ⇒ the group has no installed member and was skipped (that would be a fresh
/// `install`, not an update). Otherwise `Some(outcome)`.
///
/// [`crate::gate::decide`] is the security authority, evaluated first. The consumer gates
/// run strictly after it and can only suppress or abort, never move a build:
/// * **local pin** — if any member is pinned AND the group is not tombstoning AND it wants an
///   upgrade, the whole tuple is held on its current builds ([`TxnOutcome::Pinned`]); a
///   Tombstone anywhere makes the pin IGNORED so a revoked build never keeps running;
/// * **missing-triple hold** — if a member the group would move has a new pin proven (from
///   its verified manifest, bound to that pin) to publish no artifact for this triple, the
///   whole tuple is held on its current builds ([`TxnOutcome::Unpublished`]), unless a
///   current build is revoked, which aborts with that build's commands disabled; a
///   fetch/verify failure proves nothing and defers to the real stage;
/// * **disk preflight** — a group-aggregated shortfall aborts the group before staging so it
///   stays coherent on its current builds; fails OPEN on any query/fetch failure.
#[allow(
    clippy::too_many_arguments,
    reason = "the per-group apply needs the same irreducible inputs as apply_channel: the \
              network fetcher, the layout, the verified index + channel, the channel name + \
              triple selectors, the group, the installed-build map, and the caller's \
              resolved-asset collector the pass-end gc sparing reads"
)]
fn apply_group(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    ch: &Channel,
    channel: &str,
    triple: &str,
    group: &Group,
    installed: &BTreeMap<String, u64>,
    excluded: &[String],
    resolved_assets: &mut BTreeMap<String, String>,
) -> Option<(Group, TxnOutcome, BTreeMap<String, AppliedMember>)> {
    // `update` touches INSTALLED groups only: skip a group with no installed member (that
    // would be a fresh `install`, not an update). A coherence group with even ONE member
    // installed IS processed in full — the locked tuple must stay coherent, so a missing
    // sibling is pulled in to the pin (decide → Install).
    //
    // A TOMBSTONED MEMBER STILL COUNTS AS INSTALLED. `installed` reaches this lane from
    // [`crate::ops::active_builds`], which resolves `bin/<tool>` — and a TOMBSTONE (the
    // failing script §7 writes over a revoked build's shims) resolves to nothing, by
    // design. So a program whose commands this manager itself disabled is absent from that
    // map, and a TOMBSTONED SINGLETON — or a tuple every member of which was tombstoned —
    // was skipped here, whole and unreported. The tombstone says `was yanked/revoked — run
    // "aterm pkg update"`; the user ran exactly that; this returned `None`; the row stayed
    // `tombstoned` and every command kept failing. The self-heal `cmd_update_all_code`
    // documents (the shim view's silence makes `decide` return Install, and the transaction
    // re-stages the build `store/<p>/current` already names) was reachable ONLY when some
    // OTHER member of the same group was still installed — so the one shape the message is
    // written for, a yanked singleton whose pin has since been re-pointed at a good build,
    // was the one shape it never fixed. Only `aterm pkg update <name>` recovered it, and
    // set completion cannot stand in on a machine that installed by name and never adopted
    // the toolset.
    //
    // THE TEST IS THE TOMBSTONE, NOT THE WITNESS. A member counts here only when a shim
    // this manager wrote is still SITTING on `PATH` and refusing to run
    // ([`any_member_tombstoned_in_place`]) — never merely because the prefix still has a
    // store witness for it. A shim that is simply GONE is a different machine state (an
    // interrupted uninstall, a hand-removed file, a lane that only ever laid some names),
    // and reviving it here would make the update lane reinstall a program nobody asked it
    // to: `the_update_pass_holds_an_installed_dependent_whose_dependency_was_uninstalled`
    // pins that contract — a missing dependency is never installed by this lane, and the
    // dependent stays HELD rather than being quietly unblocked by a resurrection.
    //
    // It is also NOT the round-13 widening that was reverted, which is what makes the
    // recovery honest: that one widened `installed` ITSELF, the map handed to `decide`, and
    // so answered UpToDate forever for a tombstoned program whose pin had not moved,
    // re-pointed shims into a revoked build on rollback, and revived the set on a declined
    // machine. `installed` is untouched here. Only the question "is this group on this
    // machine at all?" reads the tombstone; `decide` still sees `None` for that member and
    // still returns Install — the re-stage the tombstone promises ([`Staged::was_live`]
    // exists for exactly this member) — and never `UpToDate`.
    //
    // Nor can it revive a removal: `ops::uninstall` deletes the shims, the store tree and
    // the links, so there is no tombstone and no witness to find, and the
    // `removed`/`declined`/`excluded` filter below still runs AFTER this and returns `None`
    // for a group left with no member. The scan runs only when the shim view was silent for
    // every member — the path that was about to skip the group anyway — so the ordinary
    // six-hourly tick pays nothing for it.
    if group.members.iter().all(|m| !installed.contains_key(m))
        && !any_member_tombstoned_in_place(layout, group)
    {
        return None;
    }
    // A DELIBERATELY REMOVED MEMBER IS EXCLUDED FROM THE TUPLE — AND NOTHING ELSE.
    //
    // `aterm pkg uninstall trust` frees ~3.2 GB, records the removal durably, and says
    // this machine no longer auto-completes the toolset. The coherence rule — a group
    // with any installed member is applied whole, missing siblings pulled in — would
    // otherwise put it straight back on the next six-hourly tick.
    //
    // FIVE INDEPENDENT DERIVATIONS OF THIS PREDICATE'S REQUIREMENTS agreed on what
    // three rounds of my own fixes kept missing: every one of them special-cased the
    // REVOCATION path, and none of them said anything about the ordinary tick, which is
    // what this code actually meets almost every time it runs. The result was a bare
    // `return None` whenever nothing was revoked — freezing the members that ARE here
    // at their current builds forever, with no report, no status row, and an aggregate
    // sentence that still read "up to date". A publisher shipping a fix as a new pin
    // (without yanking the old one, which is the normal way to ship a fix) was withheld
    // indefinitely, silently, on every machine that had ever uninstalled a member
    // (2026-08-20 independent derivation).
    //
    // So the hold does ONE thing: drop the members that are recorded-removed AND
    // ABSENT, then run the ordinary transaction on what remains. Revocation, routine
    // upgrades, the pin gate, tombstoning and reporting all take their normal path —
    // there is no second implementation of them here to get wrong. The trigger also
    // requires actual ABSENCE: a stale record for a member that a signed `requires`
    // pull-in has since reinstalled is not a reason to hold anything.
    let removed = layout.removed_programs();
    // `uninstall --all` writes `declined` — the durable "this machine does not want the
    // bundled toolset" — and clears nothing per-program. The flow layer never read it,
    // so `uninstall --all` followed by installing ONE program let the next unattended
    // pass pull the rest of its coherence tuple back, gigabytes, unannounced. A machine
    // that declined the set is not asking for the set (2026-08-20 independent
    // derivation).
    let declined = layout.declined().is_file();
    // `[packages].exclude` arrives as the `excluded` PARAMETER, the same way the
    // layout's own records do — never via `config::cached()`. That distinction is
    // load-bearing: the global is a process-wide OnceLock over the INVOKING user's
    // aterm.toml, and this layer decides against a caller-supplied `layout`. An
    // earlier wiring read the global here, which made a synthetic-layout call apply
    // some other prefix's exclusions and made this file's own unit tests depend on
    // the developer's real config — in a module that refuses to touch the real
    // `~/.aterm` for exactly that reason (2026-08-20 round-13 audit; the gap it
    // left documented is what this parameter closes). The CALLER (cli.rs) owns the
    // decision of whose config speaks.
    //
    // The absence guard applies to exclusions exactly as it does to removals:
    // exclude means "do not PULL THIS IN as a sibling", and `uninstall` names it
    // as the way to drop one program while staying adopted. A member the user has
    // since explicitly installed is present, so the ordinary update path keeps it
    // current — an exclusion never freezes or drops what is deliberately here.
    let deliberately_absent = |m: &String| {
        (declined || removed.contains(m) || excluded.contains(m)) && !installed.contains_key(m)
    };
    if group.members.iter().any(deliberately_absent) {
        let present = Group {
            members: group
                .members
                .iter()
                .filter(|m| !deliberately_absent(m))
                .cloned()
                .collect(),
            ..group.clone()
        };
        // Nothing left to act on. `transact` reports `UpToDate` for an empty decision
        // set, which would print the update lane's most reassuring line over a group
        // that was not looked at.
        if present.members.is_empty() {
            return None;
        }
        // REPORT THE GROUP THAT WAS ACTED ON, not the nominal one. The caller wrote
        // per-program status rows from the group it was handed, so a filtered tuple
        // produced an `active` row for the very program the user deleted — a phantom
        // installation in the surface the user is sent to check
        // (2026-08-20 independent derivation).
        let (outcome, applied) = apply_group_txn(
            fetcher,
            layout,
            index,
            ch,
            channel,
            triple,
            &present,
            installed,
            resolved_assets,
        );
        return Some((present, outcome, applied));
    }
    let (outcome, applied) = apply_group_txn(
        fetcher,
        layout,
        index,
        ch,
        channel,
        triple,
        group,
        installed,
        resolved_assets,
    );
    Some((group.clone(), outcome, applied))
}

/// §11 bootstrap: apply ONE coherence group all-or-nothing against a caller-resolved,
/// ALREADY-verified index — the fresh-install twin of [`apply_channel`]'s per-group body,
/// for the default-set bootstrap. Transaction semantics are IDENTICAL to the update path
/// (decide-first, local-pin hold, group-aggregated disk preflight, stage-all → flip-all →
/// rollback, abort discard) with ONE difference: the update-only "at least one member
/// installed" guard is dropped, because a bootstrap group is typically absent entirely.
/// The caller resolves + verifies the index ONCE for its whole pass and hands it in, so a
/// mid-pass index publish can never split a fresh tuple across two index states (§7) —
/// the hole a per-member `install` loop (which re-resolves each call) cannot close.
///
/// `resolved_assets` collects `program → pinned asset file name` for every member the
/// transaction resolved far enough to select an artifact — a member whose download then
/// FAILED included, which is the point: the caller's pass-end
/// [`crate::gc::run_keeping_pinned_partials`] spares that member's `.part` resume state.
/// An out-param (not part of the return) because it must survive every failure shape.
#[allow(
    clippy::too_many_arguments,
    reason = "the bootstrap group apply needs the same irreducible inputs as apply_group \
              minus the pre-resolved channel it looks up itself: fetcher, layout, verified \
              index, channel + triple selectors, the group, the installed-build map, and \
              the caller's resolved-asset collector the pass-end gc sparing reads"
)]
pub fn bootstrap_group(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    channel: &str,
    triple: &str,
    group: &Group,
    installed: &BTreeMap<String, u64>,
    resolved_assets: &mut BTreeMap<String, String>,
) -> Result<(TxnOutcome, BTreeMap<String, AppliedMember>), FlowError> {
    // The per-target view, exactly as the caller's plan and [`group_missing_triple`]
    // prescan saw it — staging the raw pin would fetch another target's build.
    let ch = index
        .channel_for(channel, triple)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    Ok(apply_group_txn(
        fetcher,
        layout,
        index,
        &ch,
        channel,
        triple,
        group,
        installed,
        resolved_assets,
    ))
}

/// §11 bootstrap prescan: `Some(member)` iff one of `members` has NO artifact for `triple`
/// in its pinned, release-verified pkg manifest — the caller then skips the WHOLE group
/// cleanly, lifting the singleton `NoArtifact` skip doctrine to coherence groups (a tuple
/// that cannot fully exist on this host is a correct state the 6h loop must not scream
/// about, not a failure). `None` ⇒ no missing triple was PROVEN: any fetch/verify/parse
/// failure defers to the real stage, which fails the transaction loudly. Verify-before-
/// parse is the shared [`verified_pkg`] sequence.
pub fn group_missing_triple(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    channel: &str,
    triple: &str,
    members: &[String],
) -> Option<String> {
    let ch = index.channel_for(channel, triple)?;
    let ch = &ch;
    for m in members {
        let Some((_, _, pkg)) = verified_pkg(fetcher, index, ch, m) else {
            continue;
        };
        if pkg.artifact_for(triple).is_none() {
            return Some(m.clone());
        }
    }
    None
}

/// The transaction body shared by [`apply_group`] (update: installed groups only) and
/// [`bootstrap_group`] (§11 fresh install): decide-first, the requirement gate
/// ([`TxnOutcome::Blocked`], §17.10), the local-pin hold, one walk over the Install
/// members' manifests feeding the missing-triple hold ([`TxnOutcome::Unpublished`], for a
/// tuple with something installed) and the group-aggregated disk preflight, then
/// stage-all → flip-all → rollback via [`transact`], with the abort discard,
/// tombstone-shim disable, and applied `tree_root` capture.
#[allow(
    clippy::too_many_arguments,
    reason = "the per-group apply needs the same irreducible inputs as apply_channel: the \
              network fetcher, the layout, the verified index + channel, the channel name + \
              triple selectors, the group, the installed-build map, and the caller's \
              resolved-asset collector the pass-end gc sparing reads"
)]
fn apply_group_txn(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    ch: &Channel,
    channel: &str,
    triple: &str,
    group: &Group,
    installed: &BTreeMap<String, u64>,
    resolved_assets: &mut BTreeMap<String, String>,
) -> (TxnOutcome, BTreeMap<String, AppliedMember>) {
    // Per member, from the COMPLETENESS-aware view ([`installed_for_decide`]): on the raw
    // shim view a member whose `<build>.ready` is missing or the other slice's is UpToDate
    // forever, so the tuple holds a tree that cannot run here and nothing re-stages it.
    let decisions: Vec<(String, ApplyDecision)> = group
        .members
        .iter()
        .map(|m| {
            let installed = installed_for_decide(layout, m, installed.get(m).copied());
            (m.clone(), decide(ch, m, installed))
        })
        .collect();

    let any_tombstone = decisions
        .iter()
        .any(|(_, d)| *d == ApplyDecision::Tombstone);
    // An UPGRADE MOVES the tuple to a different build. A member that is `Install` only
    // because its tree is not complete for this slice is re-staged AT the build the pin
    // already names — a repair, and not what a local pin may suppress: holding it would
    // freeze the tuple on a tree that cannot run here, which is not what a pin means. On
    // the shim view alone the two predicates coincide (`decide` returns UpToDate exactly
    // when the installed build equals the pin), so this is the old test plus the case the
    // completeness input introduced.
    let wants_upgrade = decisions
        .iter()
        .any(|(m, d)| *d == ApplyDecision::Install && installed.get(m) != ch.pin.get(m));
    // Whether every installed member's CURRENT build is itself still gate-valid — the
    // guard BOTH holds below must pass. If any current build is yanked or below the
    // floor, decide() returned Install to force-upgrade OFF it (not Tombstone), and a
    // hold would keep a revoked/below-floor build running; revocation outranks every
    // consumer gate, so the tuple force-upgrades to the valid pin instead.
    let all_current_valid = group
        .members
        .iter()
        .all(|m| crate::gate::current_build_ok(ch, m, installed.get(m).copied()));

    // THE REQUIREMENT GATE (§17.10) — strictly AFTER decide(), suppression-only,
    // coherence-preserving, and the ONE rule ([`crate::requires::unmet_requirement`])
    // the set-completion pass and the OS-installed reconcile apply before a member
    // starts: the group is held on its current builds while one of its `requires` (a
    // tuple's: the union of its members', minus the tuple) is not installed, dev-linked,
    // system-satisfied or installed through its protocol. Running here, in the body the
    // update lane shares with the bootstrap lane, is what makes `apply_channel` and
    // `apply_program` honour it too: an already-installed dependent whose dependency
    // was uninstalled is NOT moved to a newer pin, and nothing is resolved or downloaded
    // for it — `Blocked` names the dependency and quotes its row, the caller records
    // `blocked by <dep>: <dep state>` with the build and attestation kept, and the next
    // pass retries. Never over a Tombstone (transact tombstones the group — a revoked
    // build never keeps running) and never over a force-upgrade off a revoked current
    // build, exactly like the pin below. The plan is dependency-first, so a requirement
    // this pass could satisfy has already had its turn.
    if !any_tombstone && all_current_valid {
        let requires = crate::apply::group_requires(index, group);
        if let Some((dep, dep_state)) = crate::requires::unmet_requirement(layout, index, &requires)
        {
            return (TxnOutcome::Blocked { dep, dep_state }, BTreeMap::new());
        }
    }

    // LOCAL PIN GATE — strictly AFTER decide(), suppression-only, coherence-preserving.
    if !any_tombstone && wants_upgrade {
        // If ANY member is tombstoned the pin is IGNORED (transact tombstones the group — a
        // revoked build never keeps running). One pinned member freezes the WHOLE tuple on
        // its current builds (never splits it); nothing is staged/flipped.
        let held: Vec<String> = group
            .members
            .iter()
            .filter(|m| crate::pin::is_pinned(layout, m))
            .cloned()
            .collect();
        // CRITICAL: a hold freezes EVERY member on its current build, so it is only safe when
        // every installed member's current build is itself still gate-valid
        // (`all_current_valid` above) — honoring the pin otherwise would keep a
        // revoked/below-floor build running via a purely local pin. In that case the pin
        // is IGNORED and the tuple force-upgrades to the valid pin.
        if !held.is_empty() && all_current_valid {
            return (TxnOutcome::Pinned(held), BTreeMap::new());
        }
    }

    // One walk over the manifests of the members decide() marked Install — exactly the
    // members `transact` would stage — feeds both gates below: the missing-triple hold and
    // the group-aggregated disk preflight. It makes the requests the disk preflight always
    // made, in the same order, so the hold costs no extra fetch. Only when nonempty, so an
    // UpToDate group walks nothing and is never aborted on a low-but-nonzero disk. Manifests
    // only; the asset download stays in `stage_member`.
    let install_members: Vec<&String> = decisions
        .iter()
        .filter(|(_, d)| *d == ApplyDecision::Install)
        .map(|(n, _)| n)
        .collect();
    let need = if install_members.is_empty() {
        InstallNeed::Unknown
    } else {
        group_install_need(fetcher, index, ch, &install_members, triple)
    };

    // The missing-triple hold: the bootstrap's clean-skip doctrine ([`group_missing_triple`]
    // — a tuple that cannot fully exist on this host is a correct state, not a failure)
    // lifted to an installed tuple. Without it, a channel that moved a group to a pin the
    // publisher has not built for this triple sent the update lane into `stage_member`, whose
    // `artifact_for(triple)` miss aborted the group on every six-hourly pass while the tuple
    // kept running its previous builds.
    //
    // Strictly after decide(), suppression-only, and proof-only: the walk must show the
    // missing row in the verified manifest of a member this pass would stage, bound to that
    // member's pinned program and build. An UpToDate member is never read, and a
    // fetch/verify failure or an unbound manifest proves nothing, so the real stage still
    // runs and fails loudly. Only a group with something installed has builds to stay on, so
    // the fresh install is untouched. Never over a Tombstone, and never a quiet hold over a
    // force-upgrade off a revoked current build — that would keep the revoked build running
    // with no replacement, so its commands are disabled and the group aborts loudly.
    if !any_tombstone
        && group.members.iter().any(|m| installed.contains_key(m))
        && let InstallNeed::Unpublished { member, build } = &need
    {
        if all_current_valid {
            return (
                TxnOutcome::Unpublished {
                    member: member.clone(),
                    build: *build,
                    triple: triple.to_string(),
                },
                BTreeMap::new(),
            );
        }
        let disabled = disable_revoked_currents(layout, ch, &decisions, installed);
        for program in &disabled {
            println!(
                "{}",
                recalled_unpublished_notice(program, member, *build, triple)
            );
        }
        // The abort names the program it disabled, not the unpublished member, because that
        // row has to outlive this pass. The tombstone shims drop the disabled program from
        // `active_builds`, so the next pass sees it as not installed and the tuple takes the
        // quiet hold above, which rewrites only installed members' rows — leaving the
        // disabled program's row reading `managed`, the silent tombstone doctor exists to
        // catch. On the disabled program, the row stands until an apply replaces the
        // tombstone.
        return (
            TxnOutcome::Aborted {
                failed: disabled.first().unwrap_or(member).clone(),
                during_flip: false,
                why: format!(
                    "its current build was recalled and the group's new pin ({member} build \
                     {build}) is not published for {triple}, so its commands are disabled \
                     until it is"
                ),
            },
            BTreeMap::new(),
        );
    }

    // Group-aggregated disk preflight (§9): one all-or-nothing check for the whole tuple,
    // over the walk's aggregate. A walk that proved no aggregate fails open.
    if let InstallNeed::Bytes(required) = need
        && disk_gate(required, crate::freespace::available_bytes(&layout.prefix)).is_err()
    {
        // Stage NOTHING — the group stays coherent on its current builds. But
        // "coherent" must not mean "the revoked build keeps working": `decide` returns
        // Install to FORCE-UPGRADE off a yanked or below-floor build, so an abort here
        // leaves exactly that build runnable, on exactly the machine this path is most
        // likely to meet — the one whose owner uninstalled a multi-gigabyte sibling to
        // reclaim space. The upgrade is what could not be afforded; disabling the
        // revoked build costs nothing and is the half of the decision that must still
        // happen (2026-08-20 independent derivation).
        let disabled = disable_revoked_currents(layout, ch, &decisions, installed);
        // SAY IT. Disabling a program's tools is the loudest thing this pass can do to a
        // machine, and it was the only tombstone site in this file that reported
        // nothing — while the abort message immediately below told the user the group
        // "stays coherent on its previous builds", which is the opposite of what just
        // happened to these members (2026-08-20 round-13 audit).
        for program in &disabled {
            println!(
                "atpkg: {program} was recalled and there is not room to install its \
                 replacement — its commands are disabled until there is. Free space and \
                 run `aterm pkg update`."
            );
        }
        // Stage NOTHING — the group stays coherent on its current builds.
        return (
            TxnOutcome::Aborted {
                failed: install_members[0].clone(),
                during_flip: false,
                why: format!(
                    "not enough free disk for the group ({} needed for its downloads and \
                     staging plus the free floor)",
                    crate::cost::human_bytes(required)
                ),
            },
            BTreeMap::new(),
        );
    }

    // Per-group transaction. `staged` is filled by the stage closure and read by flip/rollback.
    let staged: RefCell<BTreeMap<String, Staged>> = RefCell::new(BTreeMap::new());
    let outcome = transact(
        &decisions,
        &mut |m| {
            let mut why = String::new();
            match stage_member(
                fetcher,
                layout,
                index,
                ch,
                m,
                triple,
                installed.get(m).copied(),
                resolved_assets,
                &mut why,
            ) {
                Some(s) => {
                    staged.borrow_mut().insert(m.to_string(), s);
                    Ok(())
                }
                None => Err(if why.is_empty() {
                    String::from("the member could not be staged (no reason recorded)")
                } else {
                    why
                }),
            }
        },
        &mut |m| {
            // The flip is the group lane's link phase (label-only, per the schema).
            crate::progress::note_phase(m, crate::progress::Phase::Link);
            staged
                .borrow()
                .get(m)
                .is_some_and(|s| flip_member(layout, channel, m, s))
        },
        &mut |m| {
            if let Some(s) = staged.borrow().get(m) {
                rollback_member(layout, channel, m, s);
            }
        },
    );
    // On abort, DISCARD the builds this transaction staged. They were never left active (a
    // stage-phase abort flipped nothing; a flip-phase abort re-pointed every shim back to the
    // prior build via rollback), so leaving a complete-but-inactive build on disk would make
    // list_installed/`decide` mis-read it as the active build next run — silently splitting
    // the coherence tuple and wedging the update while reporting success. Within THIS
    // process the staged `build_dir` is USUALLY the NEW pinned build — but not always, and
    // the earlier claim that it always is ("decide only stages Install members, so new !=
    // the prior active build") was WRONG: `decide` reads the SHIM-derived `installed` view
    // ([`crate::ops::active_builds`]), which goes SILENT when a program's tools are all
    // tombstoned or `atpkg unlink` removed its dev shims, so it legitimately returns Install
    // for the build `store/<program>/current` already names. `Staged::was_live` catches
    // exactly that member below. Across processes the discard rests on the
    // SINGLE-WRITER-PER-STORE contract ([`crate::lock`]): every mutating verb try-acquires
    // the store-wide `store.lock` at the CLI edge, so no OTHER atpkg process can be staging
    // or activating builds in this store while this transaction runs — without that lock, a
    // concurrent process could have just activated one of these very builds, and this
    // discard would leave its shims dangling on a deleted tree.
    if matches!(outcome, TxnOutcome::Aborted { .. }) {
        for s in staged.borrow().values() {
            // NEVER delete a build that was already LIVE when this transaction re-staged it.
            // [`crate::gc::live_builds`] calls exactly that build live and protects it;
            // deleting it here leaves both `current` links dangling and forces a full
            // re-download of a multi-GB toolchain — triggered by the very network failure
            // that aborted the group and that makes re-downloading impossible.
            if s.was_live {
                continue;
            }
            crate::store::discard_build(&s.build_dir);
        }
    }
    // …but NOT their downloads. The compressed archive of every member that staged is
    // reclaimed HERE, once the tuple's fate is known, instead of at the end of each
    // member's own stage ([`carried_archive`], `stage_member`):
    //
    //   - settled (applied/tombstoned/up-to-date): the group is done with these bytes and
    //     nobody ever comes back for them, so they go exactly as they always did;
    //   - ABORTED: they are KEPT. The next pass re-stages every member of this tuple, and
    //     each kept archive is one multi-gigabyte download that pass does not repeat. The
    //     abort above already took the extracted trees, which is the invariant that matters
    //     (an inactive complete build would split the tuple); an archive is inert — nothing
    //     resolves, executes or `list_installed`s into `staging/`.
    //
    // The strandage bound is the `.part` bound, by the same argument: the pass-end
    // `gc::run_keeping_pinned_partials` spares `<asset>` beside `<asset>.part` for exactly
    // the programs this pass resolved (`ChannelApplyReport::resolved_assets`), so at most
    // one file of each name per program survives a pass; a plain `atpkg gc` reclaims them
    // whenever the user asks; and all of this runs under the store-wide writer lock. A
    // member that FAILED already reclaimed its own archive inside `stage_member`, so this
    // loop can only ever remove — it never resurrects a bad asset.
    if !matches!(outcome, TxnOutcome::Aborted { .. }) {
        for program in staged.borrow().keys() {
            if let Some(asset) = resolved_assets.get(program)
                && let Ok(dl) = staged_download_path(layout, program, asset)
            {
                let _ = std::fs::remove_file(&dl);
            }
        }
    }
    // A tombstoned member's OLD working shims must be actively DISABLED, not just reported
    // (§7). `transact` returns Tombstoned when decide() tombstoned any member; install a
    // failing tombstone shim over each such program's currently-exposed tools so a revoked
    // build is not left runnable. Best-effort — the report is still the source of truth.
    if let TxnOutcome::Tombstoned(members) = &outcome {
        for m in members {
            install_tombstone_shims(layout, m, installed.get(m).copied());
        }
    }
    // Capture the SIGNED tree_root of each LIVE-flipped member so the CLI can record it for
    // `atpkg verify` (the `staged` map still holds this group's entries).
    let mut applied: BTreeMap<String, AppliedMember> = BTreeMap::new();
    if let TxnOutcome::Applied(members) = &outcome {
        for m in members {
            if let Some(s) = staged.borrow().get(m) {
                applied.insert(
                    m.clone(),
                    AppliedMember {
                        build: s.build,
                        tree_root: s.tree_root.clone(),
                    },
                );
            }
        }
    }
    (outcome, applied)
}

/// Is `dl` a COMPLETE, already-verified copy of `artifact`'s compressed asset — an
/// archive an earlier pass downloaded and this one may stage without refetching?
///
/// # What this bounds
///
/// A coherence group aborts all-or-nothing, and the abort DISCARDS every staged sibling's
/// extracted tree (`apply_group_txn`'s abort discard — a complete-but-inactive build would
/// make `list_installed`/`decide` mis-read it as the active build and silently split the
/// tuple). That invariant is untouched. What must NOT go with the tree is the DOWNLOAD:
/// before this gate, a 4-member tuple that failed on member 3 threw away members 1-2 and
/// refetched them from byte 0 on the next tick, so the resume-across-passes guarantee (R4)
/// covered only the member that actually failed, and a multi-gigabyte tuple on a flaky
/// link could never converge. The archive of a member that staged is therefore kept until
/// the tuple's fate is known, and this is what the next pass admits it back through.
///
/// # Trust posture
///
/// A REUSE gate, never a verification shortcut — it can only ever SKIP A DOWNLOAD, never
/// admit bytes. The file is accepted only when its SHA-256 equals the SIGNED `sha256`, so
/// the bytes it stands for are by construction the ones the fetcher would have produced;
/// `verify_and_stage` then re-checks that same digest AND the signed `tree_root` after
/// extraction, exactly as it does for a freshly downloaded archive. An empty signed
/// `sha256` (no digest to match) is refused, and so is anything that is not a regular
/// file. A file that fails is not reused: the caller removes it and fetches, which is the
/// old behaviour unchanged.
///
/// The digest is the whole gate — deliberately NOT pre-filtered on the signed `size`,
/// which is the download CAP and not a promise about a file already on disk. Reading the
/// file to hash it is the only cost, it is paid only when an archive is actually sitting
/// there (a settled tuple leaves none), and it buys a multi-gigabyte transfer.
fn carried_archive(dl: &Path, artifact: &crate::manifest::Artifact) -> bool {
    if artifact.sha256.is_empty() {
        return false;
    }
    let Ok(meta) = std::fs::symlink_metadata(dl) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    // A hard link into the sealed registry (a `dir:` registry's stale staging entry) is
    // never carried: it is the app bundle's own inode, and reading it as the download
    // would be right while any later write through the name would be into the bundle
    // (2026-09-15) — one link, one owner, or it is fetched afresh.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.nlink() != 1 {
            return false;
        }
    }
    crate::tree::file_sha256(dl).is_ok_and(|got| got.eq_ignore_ascii_case(&artifact.sha256))
}

/// Disable the live commands of every member `decide` asked to force off its current build
/// — an Install whose current build is yanked or below the floor — by laying failing
/// tombstone shims over them, and return their names. An abort that leaves the tuple on its
/// current builds (a disk shortfall, a replacement not published for this triple) must not
/// leave a revoked build runnable.
fn disable_revoked_currents(
    layout: &Layout,
    ch: &Channel,
    decisions: &[(String, ApplyDecision)],
    installed: &BTreeMap<String, u64>,
) -> Vec<String> {
    let disabled: Vec<String> = decisions
        .iter()
        .filter(|(p, d)| {
            *d == ApplyDecision::Install
                && !crate::gate::current_build_ok(ch, p, installed.get(p).copied())
        })
        .map(|(p, _)| p.clone())
        .collect();
    for program in &disabled {
        install_tombstone_shims(layout, program, installed.get(program).copied());
    }
    disabled
}

/// The line the missing-triple hold prints for each `program` it disables: the build this
/// `triple` lacks — `member`'s pinned `build`, the tuple's new pin — and that the program's
/// commands stay disabled until it publishes. The two names differ whenever a sibling's new
/// pin is the missing one: the recalled program's own replacement may be published.
pub(crate) fn recalled_unpublished_notice(
    program: &str,
    member: &str,
    build: u64,
    triple: &str,
) -> String {
    if program == member {
        format!(
            "atpkg: {program} was recalled and its replacement ({program} build {build}) is \
             not published for {triple} — its commands are disabled until it is."
        )
    } else {
        format!(
            "atpkg: {program} was recalled and its coherence group's new pin ({member} build \
             {build}) is not published for {triple} — its commands are disabled until it is."
        )
    }
}

/// Reclaim every `*.part` in `dl`'s directory EXCEPT `dl`'s own.
///
/// A resumable artifact download keeps its partial across attempts, so the partial for
/// the asset we are about to fetch must survive; one for an asset this program has moved
/// past (a superseded build, a renamed artifact) never will be finished by anyone. The
/// bound this establishes is "at most one stranded partial per program between passes",
/// instead of one per abandoned build — `gc`'s `staging/` sweep is the other reclaimer,
/// but it only runs when the user runs it.
///
/// Deliberately narrow: only regular files whose name ends in `.part`, only in `dl`'s own
/// directory, never recursive. `dl` itself and any other file are untouched.
/// Reclaim `dl`'s own sibling `<asset>.part`, if one exists.
///
/// The belt-and-braces arm of the digest gate: an asset that just FAILED its signed
/// sha256 must not leave a partial behind to seed the next attempt — a poisoned prefix
/// would cost that attempt a full download plus a second guaranteed mismatch. Nearly
/// vacuous on the resumable lane (the fetcher renamed the `.part` onto the asset before
/// verify ran, so a mismatch-failing download usually has no sibling partial) — this is
/// a cheap invariant, not a load-bearing gate; the load-bearing gate is the sha256
/// check itself, which is untouched.
fn discard_sibling_partial(dl: &Path) {
    let Some(name) = dl.file_name() else {
        return;
    };
    let mut part = name.to_os_string();
    part.push(".part");
    let _ = std::fs::remove_file(dl.with_file_name(part));
}

fn sweep_foreign_partials(dl: &Path) {
    let (Some(dir), Some(name)) = (dl.parent(), dl.file_name()) else {
        return;
    };
    let mut keep = name.to_os_string();
    keep.push(".part");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let entry_name = entry.file_name();
        if entry_name == keep || entry_name == name {
            continue;
        }
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let is_part = std::path::Path::new(&entry_name)
            .extension()
            .is_some_and(|ext| ext == "part");
        // A `.part` of another asset, or — since archives of a failed stage are
        // RETAINED for reuse ([`archive_reusable`]) — a full archive of another asset
        // name: at most one of each survives per program, and it is this pass's.
        let is_archive = std::path::Path::new(&entry_name)
            .to_str()
            .is_some_and(|n| n.ends_with(".tar.zst") || n.ends_with(".dmg"))
            || entry_name
                .to_str()
                .is_some_and(|n| n.starts_with("claude-") || n.starts_with("codex-"));
        if is_part || is_archive {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// What a FAILED stage leaves in `staging/`: nothing for a digest failure (the bytes
/// are wrong — the archive and any `.part` that would seed the next attempt go, see
/// [`discard_sibling_partial`]); the verified archive, kept in place, for every other
/// failure, so the next attempt stages it again without the download
/// ([`carried_archive`]). One archive per program at most survives —
/// [`sweep_foreign_partials`] removes the others — so a member that keeps failing keeps
/// one copy, never one per attempt.
fn reclaim_after_failed_stage(dl: &Path, e: &StageError) {
    if matches!(e, StageError::Sha256Mismatch { .. }) {
        let _ = std::fs::remove_file(dl);
        discard_sibling_partial(dl);
    }
}

/// Record a stage failure beside the build it was staging, so a pass that keeps meeting
/// the same one stops paying the whole download to be told so
/// ([`crate::store::StageRefusal`]).
///
/// EXACTLY ONE failure is recorded — the signed-`sha256` mismatch — because it is the only
/// one whose retry costs a TRANSFER. It is the failure that (rightly) deletes the archive
/// and its sibling partial: the bytes are wrong, and a poisoned prefix must never seed the
/// next attempt, so the next attempt starts from byte 0.
///
/// A `tree_root` mismatch is deliberately NOT recorded, though it is just as
/// deterministic: its archive passed the signed digest and is KEPT, so the next attempt
/// re-stages it with no download at all ([`carried_archive`], pinned by
/// `a_failed_stage_retains_one_verified_archive_and_a_digest_failure_none`). There is no
/// transfer there to spare, and refusing it would only break that reuse. Every other
/// failure is a fact about THIS MACHINE (a full disk, an unreadable store, a refused
/// installer lane), keeps its verified archive for the same reason, and retries freely.
fn record_digest_refusal(build_dir: &Path, artifact: &crate::manifest::Artifact, e: &StageError) {
    if !matches!(e, StageError::Sha256Mismatch { .. }) || artifact.sha256.is_empty() {
        return;
    }
    // Best-effort: a store this process cannot write is a machine fault, and the pass
    // that could not record the refusal still reports the stage failure itself.
    let _ = crate::store::record_stage_refusal(
        build_dir,
        &artifact.sha256,
        &artifact.tree_root,
        &e.to_string(),
        now_unix(),
    );
}

/// The sentence a still-binding refusal ([`record_digest_refusal`]) owes this pass, or
/// `None` when there is nothing recorded, the pin's signed digests have moved, or the
/// cooldown has lapsed — in which case the caller downloads exactly as it always did.
///
/// The WALL clock, deliberately, and not the `now_unix` the freshness gates take: this is
/// a bandwidth cooldown, never a trust decision, and it is the only thing in this file a
/// memo can influence. [`now_unix`] fails closed to `i64::MAX`, which makes every memo
/// read as lapsed — an unreadable clock can therefore only cost a download, never block
/// an install.
fn digest_refusal_note(
    layout: &Layout,
    program: &str,
    build: u64,
    artifact: &crate::manifest::Artifact,
) -> Option<String> {
    let now = now_unix();
    let memo = crate::store::stage_refusal(&layout.build_dir(program, build))?;
    if !memo.binds(&artifact.sha256, &artifact.tree_root, now) {
        return None;
    }
    let hours = memo
        .retry_after()
        .saturating_sub(now)
        .saturating_add(3599)
        .div_euclid(3600)
        .max(1);
    // Built by hand rather than wrapped into one `format!` string: the sentence is long,
    // and a continuation inside a string literal is how it grows a run of spaces nobody
    // sees until it is printed at an operator.
    let mut note = String::from(program);
    note.push_str(" build ");
    note.push_str(&crate::dec_u64(build));
    note.push_str(": ");
    note.push_str(&crate::dec_u64(u64::from(memo.attempts)));
    note.push_str(" attempts over the identical signed digests proved the published ");
    note.push_str(&artifact.asset);
    note.push_str(" does not match them (");
    note.push_str(&memo.why);
    note.push_str(") — not refetching ");
    note.push_str(&crate::cost::human_bytes(artifact.size));
    note.push_str(" to reach the same verdict. The next attempt is due when the pin or ");
    note.push_str("its signed digests change, or in about ");
    note.push_str(&crate::dec_u64(u64::try_from(hours).unwrap_or(0)));
    note.push_str("h; `aterm pkg install ");
    note.push_str(program);
    note.push_str("` retries now.");
    Some(note)
}

/// Pure disk gate (§9): `Ok(())` unless `available` is a measured value that fails
/// [`crate::cost::disk_ok`] against `required` + the [`crate::cost::FREE_FLOOR`]. `None`
/// available (the query failed) fails OPEN — preflight is a safety net, not a security gate.
/// `available` is injected so the gate is unit-testable without a real `statvfs`.
fn disk_gate(required: u64, available: Option<u64>) -> Result<(), FlowError> {
    match available {
        Some(avail) if !crate::cost::disk_ok(required, avail, crate::cost::FREE_FLOOR) => {
            Err(FlowError::InsufficientDisk {
                required,
                available: avail,
            })
        }
        _ => Ok(()), // None (query failed) => fail OPEN
    }
}

/// What one walk over a group's Install members' pinned, release-verified manifests proved
/// — the one walk both the missing-triple hold and the disk preflight read.
enum InstallNeed {
    /// Every Install member's manifest carries an artifact for the triple: the aggregate
    /// installed-bytes the disk preflight gates on.
    Bytes(u64),
    /// `member`'s manifest — bound to its pinned program and `build` — carries no artifact
    /// for the triple: that pin cannot exist on this host.
    Unpublished { member: String, build: u64 },
    /// Nothing proven: a fetch/verify/parse failure, or a manifest with no row for the
    /// triple that does not bind to its pin. Both gates fail open, letting the real stage
    /// surface the failure.
    Unknown,
}

/// The aggregate installed-bytes a group's Install members need — the sum of each member's
/// signed `size` (compressed asset) + `disk_installed` (extracted tree) — walked in order.
/// Verify before parse, through the shared [`verified_pkg`] sequence. Stops at the first
/// member whose manifest fails to fetch, verify or parse ([`InstallNeed::Unknown`]) or has
/// no artifact for `triple`: that is [`InstallNeed::Unpublished`] only when the manifest
/// binds to the member's pinned program and build, and [`InstallNeed::Unknown`] when it does
/// not, because another program's or build's manifest says nothing about this pin.
fn group_install_need(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    ch: &Channel,
    install_members: &[&String],
    triple: &str,
) -> InstallNeed {
    let mut total = 0u64;
    for &m in install_members {
        let Some((pinned, _, pkg)) = verified_pkg(fetcher, index, ch, m) else {
            return InstallNeed::Unknown;
        };
        let Some(a) = pkg.artifact_for(triple) else {
            return if pkg.is_for(m) && pkg.build_number == pinned {
                InstallNeed::Unpublished {
                    member: m.clone(),
                    build: pinned,
                }
            } else {
                InstallNeed::Unknown
            };
        };
        total = total
            .saturating_add(a.size)
            .saturating_add(a.cost.disk_installed);
    }
    InstallNeed::Bytes(total)
}

/// Resolve + verify-select the SIGNED index (cached-fallback, §14) and enforce
/// its freshness window (§8) — the shared prologue of every flow entry point,
/// exposed so the CLI's default-set bootstrap (§11) can read the verified
/// program set itself (`Index::installable`) without re-implementing the gate
/// order. Verify-before-parse and the floor are IDENTICAL to `install`/
/// `apply_channel`; this returns the index and installs nothing.
pub fn resolve_verified_index(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    floor: BuildFloor,
    now_unix: i64,
) -> Result<TrustedIndex, FlowError> {
    let candidates = resolve_candidates(fetcher, layout)?;
    verify_select_fresh(layout, anchor, candidates, floor, now_unix)
}

/// The verify-select + freshness half of [`resolve_verified_index`], over caller-supplied
/// `candidates` — shared with the single-program paths ([`rollback`], [`apply_program`],
/// [`plan_update`]), which resolve theirs through `resolve_candidates_live`: the cheap
/// identity hit path, and no §14 cached FALLBACK when the fetch fails.
/// Freshness (§8 gate 2): refuse a selected index whose window has lapsed; a
/// `valid_until` we cannot parse is treated as lapsed (fail closed).
///
/// # The roster ratchet turns HERE, on observation
///
/// The durable `roster_seq` high-water advances to the newest generation this pass ADMITTED,
/// before the freshness gate and before any caller decides whether to install. That is the
/// only ordering under which the replay defence works, and it is the one `aterm-update`'s
/// sibling tier already uses: a client that merely SAW generation *n* must refuse *n-1*
/// forever after, whether or not it went on to install anything. Ratcheting after a
/// completed install instead — which is what atpkg did — left every no-install outcome
/// (a local pin holding, a staging failure an attacker can induce, a plan that decided
/// there was nothing to do) with a floor BELOW a revocation it had already verified, and
/// the still-genuine pre-revocation roster then re-authorized the revoked machine.
///
/// Raising it here cannot lock the client out of a document it is still meant to use:
/// `observed_roster_seq` is only ever a generation that was master-signed, fresh, and
/// already at-or-above the current floor, and the ratchet refuses only STRICTLY older ones.
/// Best-effort, like every other floor write — a failed write leaves the older floor, which
/// is the direction that refuses nothing it should accept.
fn verify_select_fresh(
    layout: &Layout,
    anchor: &Anchor,
    candidates: Vec<Candidate>,
    floor: BuildFloor,
    now_unix: i64,
) -> Result<TrustedIndex, FlowError> {
    let pass = select_index(anchor, candidates, floor, now_unix);
    observe_roster_generation(layout, pass.observed_roster_seq);
    let selected = pass.selected.ok_or(FlowError::NoIndex)?;
    let index = selected.index;
    if !index_is_fresh(&index, now_unix) {
        return Err(FlowError::Stale);
    }
    Ok(index)
}

/// Durably record that this client has ADMITTED roster generation `seq` — the replay
/// ratchet's write half (`<prefix>/roster.floor`).
///
/// `0` means no generation was admitted at all (an unarmed anchor, a suppressed or
/// unverifiable roster), and writing it would be meaningless; the guard keeps a refused
/// pass from touching the file at all. Everything else is a master signature this process
/// checked itself.
pub(crate) fn observe_roster_generation(layout: &Layout, seq: u64) {
    if seq == 0 {
        return;
    }
    // The discarded Result is the accept/refuse DECISION (a Rollback here just means a
    // concurrent pass already recorded something newer — nothing to act on). A failure
    // to PERSIST the advance is not discarded: `check_and_record` reports it on stderr
    // itself, so a standing replay window is never silent.
    let _ = crate::sig::Floor::new(layout.roster_floor()).check_and_record(seq);
}

/// The §8 gate-2 freshness predicate over a verified index: whether its signed
/// `valid_until` window is still open at `now_unix`. A `valid_until` we cannot parse
/// is lapsed (fail closed).
///
/// (This used to say it was shared with "the CLI's seed-as-update-source
/// admission". No such admission has ever existed: the seed is a BOOTSTRAP
/// source only — `seed_bootstrap_leg` joins it to the chain solely on an empty
/// store — and it was not restored as one when the lane came back in 2026-08-17.)
pub(crate) fn index_is_fresh(index: &Index, now_unix: i64) -> bool {
    matches!(
        rfc3339_to_unix(&index.valid_until),
        Some(until) if crate::sig::check_freshness(now_unix, until).is_ok()
    )
}

/// Resolve the index candidates with a SAME-SOURCE cached fallback (§14): a successful
/// NON-EMPTY fetch refreshes the cache; a fetch failure — or an EMPTY success, which is a
/// fetch that FOUND nothing (an index tag pushed off the release listing, a repo with no
/// index release) and previously bypassed the fallback into a repo-wide `NoIndex` while a
/// good cache sat on disk — falls back to the last cached candidates FOR THE SAME SOURCE
/// (a `dir:` cache never satisfies a failed `github:` fetch). Both the write and the load
/// are keyed by [`Fetcher::cache_source_id`], and the write persists only
/// [`Fetcher::cacheable_candidates`] — the NETWORK leg of a chained fetcher — so a seed-leg
/// success can never overwrite the last-good network cache (cache masking, 2026-07-30).
/// Cached bytes are RAW — everything downstream (verify-then-select, freshness, floor)
/// is unchanged, so a tampered/stale cache installs nothing the live path wouldn't.
fn resolve_candidates(fetcher: &dyn Fetcher, layout: &Layout) -> Result<Vec<Candidate>, FlowError> {
    let cache = crate::cache::IndexCache::for_layout(layout);
    let src = fetcher.cache_source_id();
    // THE HIT PATH (see `Fetcher::index_identities`). Ask the cheap question first: is
    // the source still publishing the very assets the cached bytes came from? On the
    // production fetcher that question is answered by the release LISTING, which
    // `index_candidates` fetches and memoizes anyway — so this costs zero extra requests
    // whether it hits or misses, and when it hits it removes the sixteen asset downloads
    // that were the entire cost of a no-op update pass.
    //
    // WHAT THIS DOES NOT CHANGE, and the reason it is not a trust decision: the bytes
    // returned here are the same raw candidate bytes `load` has always been allowed to
    // return on a failed fetch. They go straight into `select_index` → `admit_roster` →
    // `authorize_index` → the durable `index_build` floor → the `roster_seq` ratchet →
    // the `valid_until` freshness window, every one of them unchanged. A stale or
    // tampered cache installs nothing the live path would not, exactly as §14 already
    // states — the identity match only decides whether re-fetching provably-identical
    // bytes is worth sixteen round-trips.
    //
    // A host that wants to hold a client on an old index does not need this seam: it can
    // simply keep serving the old assets, which is the suppression the freshness window
    // bounds. And a LOCAL attacker who can write `<prefix>/index-cache.toml` (0700,
    // owner-only) already owns the store the shims point into.
    //
    // `unwrap_or_default()` collapses "no cheap answer" to an empty vector, which
    // `load_if_identical` refuses outright — so every non-participating fetcher (the seed
    // `DirFetcher`, `ChainFetcher`, every test double) takes the historical path below,
    // byte for byte.
    let live = fetcher.index_identities().unwrap_or_default();
    if let Some(hit) = cache.load_if_identical(&src, &live) {
        // The identities came off the LIVE listing: the source was reached, and the
        // cache is proven current — the one cache read that is not a fallback.
        note_resolve(true, None);
        return Ok(hit);
    }
    match fetcher.index_candidates() {
        Ok(c) if !c.is_empty() => {
            match fetcher.cacheable_candidates(&c) {
                // `store` itself refuses an empty set, so a network leg that succeeded
                // with nothing keeps the older good cache rather than clobbering it.
                // An EMPTY cacheable set takes the union arm below for the same reason
                // `None` does: "the network found no index" and "the network could not
                // be reached" both mean the authoritative leg said nothing this pass.
                Some(cacheable) if !cacheable.is_empty() => {
                    // Stamped with the identities probed ABOVE — the same listing that
                    // produced these bytes, so the pairing cannot straddle a publish. A
                    // fetcher that gave no identities stores none (`&[]`), and the entry
                    // stays the failure-time fallback it has always been.
                    cache.store(&src, &cacheable, &live);
                    note_resolve(true, None);
                    Ok(c)
                }
                // The CACHEABLE (network) leg contributed NOTHING, yet the fetch as a
                // whole succeeded — only a chained fetcher can be in this state, and it
                // means the seed leg alone answered. Closing the cache-masking tooth on
                // the WRITE side is not enough here: leaving this as a plain `Ok` also
                // masks the cache READ, because the fallback below is the only place the
                // cache is consulted. That is the same defect the 2026-07-30 review
                // named, one arm over.
                //
                // It is not theoretical. On an EMPTY store the durable `index_build`
                // floor cannot rise (`advance_floors` runs only after a completed
                // install), and the empty store is exactly when the seed leg is chained
                // in — so an offline launch could resolve the SEALED index while a
                // strictly newer, already-verified network index sat in the cache, and
                // reinstate pins that index had yanked or floored out.
                //
                // So: UNION the last-good network candidates in and let the ordinary
                // monotonic selection decide. The cache is not trusted here any more
                // than anywhere else — these are raw bytes that still face
                // verify-then-select, freshness, and the floor.
                _ => {
                    // Cached candidates FIRST: `select_index` replaces only on a
                    // STRICTLY greater index_build, so on a tie the last-good network
                    // index outranks an equal-build seal — the same authority ordering
                    // the live chain uses.
                    note_resolve(
                        false,
                        Some(String::from(
                            "the network leg contributed no index; the sealed seed answered",
                        )),
                    );
                    let mut merged = cache.load(&src).unwrap_or_default();
                    merged.extend(c);
                    Ok(merged)
                }
            }
        }
        // Reached the source, and it genuinely carried no index — a repo with no
        // index release, or a tag pushed off the listing. A trust-shaped answer is
        // the right one here.
        Ok(_) => {
            note_resolve(true, None);
            cache.load(&src).ok_or(FlowError::NoIndex)
        }
        // Could NOT reach it. Keep the reason: telling an offline user their
        // signatures failed sends them to the wrong problem entirely.
        Err(why) => {
            note_resolve(false, Some(why.clone()));
            cache.load(&src).ok_or(FlowError::Unreachable(why))
        }
    }
}

/// The identity HIT PATH alone, for the lanes a TYPED single-program verb takes
/// ([`rollback`], [`apply_program`], [`plan_update`]): serve the §14 cache when the source
/// is provably still publishing the very assets those bytes came from, and otherwise fetch
/// live — with NO cached fallback if that fetch fails.
///
/// # Why this is not the §14 fallback these lanes deliberately refuse
///
/// Their rule is about FAILURE: a transient index-fetch failure must surface as
/// [`FlowError::NoIndex`], never as an install decided from a cache the network could not
/// corroborate this pass. That rule is untouched, in both directions. A listing that cannot
/// be reached answers no identities ([`Fetcher::index_identities`] is `None`, hence empty),
/// `load_if_identical` refuses an empty `live` outright, and the fetch below then fails into
/// `NoIndex` exactly as before. The hit fires only when the listing WAS reached and reported
/// the same assets, position for position — which is why it is a proof of currency and not a
/// fallback at all.
///
/// # What it buys
///
/// The sixteen downloads. Without it a typed `aterm pkg update <program>` (or `rollback`)
/// re-fetched the whole candidate set in every fresh process — on the production fetcher four
/// assets for each of `INDEX_CANDIDATE_CAP` releases, sequential `curl` subprocesses each
/// paying its own DNS+TLS handshake — to obtain bytes the 6-hourly pass had already stamped as
/// identical in `<prefix>/index-cache.toml`. The listing that answers the identity probe is the
/// one `index_candidates` would have fetched anyway, so the probe cannot ADD a round-trip.
///
/// Deliberately NOT here: the cache WRITE and the `note_resolve` provenance stamp. These lanes
/// have never refreshed the cache nor recorded what the resolve learned about the network (only
/// the pass lanes stamp `status.toml` from [`last_resolve`]), and a hit path is not the place to
/// hand them either.
fn resolve_candidates_live(
    fetcher: &dyn Fetcher,
    layout: &Layout,
) -> Result<Vec<Candidate>, FlowError> {
    let cache = crate::cache::IndexCache::for_layout(layout);
    let live = fetcher.index_identities().unwrap_or_default();
    if let Some(hit) = cache.load_if_identical(&fetcher.cache_source_id(), &live) {
        return Ok(hit);
    }
    fetcher.index_candidates().map_err(|_| FlowError::NoIndex)
}

/// What the last index resolve in this process learned about the NETWORK: whether the
/// source's listing was reached, and if not why — so a pass that ran on the §14 cache
/// can SAY so instead of recording a green "up to date". Until 2026-09-15 a rate-limited
/// or unreachable listing served the cache silently and every surface stayed green
/// while the managed `claude` could freeze at an old pin (audit 2026-09-14); the pass
/// end reads this ([`last_resolve`]) and stamps `status.toml`'s freshness fields, which
/// `aterm pkg doctor` turns into "the listing has not been reached for N days".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveProvenance {
    /// The source's release listing answered over the network this resolve.
    pub reached: bool,
    /// Why it did not, when it did not — the transport's own sentence.
    pub why: Option<String>,
}

static LAST_RESOLVE: std::sync::Mutex<Option<ResolveProvenance>> = std::sync::Mutex::new(None);

fn note_resolve(reached: bool, why: Option<String>) {
    if let Ok(mut slot) = LAST_RESOLVE.lock() {
        *slot = Some(ResolveProvenance { reached, why });
    }
}

/// The provenance of the last [`resolve_verified_index`] in this process, `None` before
/// the first.
#[must_use]
pub fn last_resolve() -> Option<ResolveProvenance> {
    LAST_RESOLVE.lock().ok().and_then(|slot| slot.clone())
}

/// Roll `program` back to the highest RETAINED build strictly below its current active build
/// that STILL passes the floor/yank gate (§9/§11). Re-points its shims + the channel
/// `current` to that build via the tested [`rollback_member`] primitive — creating/removing
/// ONLY symlinks, never mutating any retained build's extracted tree (its signed `tree_root`
/// is untouched). The index is resolved + verify-selected so the floor/yank state is
/// authoritative: the target predicate is EXACTLY the one [`decide`] tombstones on (at/above
/// THIS program's [`Channel::min_build_for`] AND not yanked), so a rollback can never land
/// below the floor or on a revoked build — it errors instead.
#[allow(
    clippy::too_many_arguments,
    reason = "rollback needs the fetcher, layout, pinned root key, channel + program \
              selectors, and the floor + clock the freshness/floor gates read — the same \
              irreducible set the install/apply entry points take"
)]
pub fn rollback(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    channel: &str,
    program: &str,
    floor: BuildFloor,
    now_unix: i64,
) -> Result<RollbackReport, FlowError> {
    // 1. The ACTIVE build (shim-derived), never a merely-staged one.
    let current = *crate::ops::active_builds(layout)
        .get(program)
        .ok_or_else(|| {
            FlowError::Rollback(format!(
                "{program} is not installed/active (aterm pkg list shows what is)"
            ))
        })?;
    // 2. Resolve + verify-select the SIGNED index so the floor/yank gate is authoritative
    //    (hit path only — the single-program paths take no §14 cached fallback on failure).
    let candidates = resolve_candidates_live(fetcher, layout)?;
    let index = verify_select_fresh(layout, anchor, candidates, floor, now_unix)?;
    // 3. Reachability — capture the coherence group for the report/warn.
    let coherence_group = index
        .program(program)
        .ok_or_else(|| FlowError::NotReachable(program.to_string(), index_roster(&index)))?
        .coherence_group
        .clone();
    // 4. The channel supplies the authoritative min_build + yank list.
    let ch = index
        .channels
        .iter()
        .find(|c| c.name == channel)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    // 5. Retained builds strictly below current, highest first.
    let mut lower: Vec<u64> = crate::ops::list_installed(layout)
        .into_iter()
        .filter(|(p, _)| p == program)
        .map(|(_, b)| b)
        .filter(|&b| b < current)
        .collect();
    lower.sort_unstable();
    lower.dedup();
    // 6. THE gate-valid selection: highest build below current that STILL passes the SAME
    //    predicates decide() tombstones on (>= THIS PROGRAM's floor AND not yanked). The
    //    floor is `min_build_for`, never the channel-wide number: a floor published to
    //    revoke another program's builds must not refuse every retained build of this one.
    let floor_for_program = ch.min_build_for(program);
    let target = lower
        .iter()
        .rev()
        .copied()
        .find(|&b| {
            b >= floor_for_program
                && !crate::gate::is_yanked(ch, program, b)
                && build_can_be_rolled_onto(layout, program, b)
        })
        .ok_or_else(|| {
            FlowError::Rollback(format!(
                "no retained build below {current} that satisfies the floor/yank gate \
                 and still holds the tools it was installed with"
            ))
        })?;
    // 7. Re-point via the tested primitive (symlinks only; no tree mutation). reloc:None —
    //    a self-contained bundle needs no pre-activation wiring to re-run.
    let staged = Staged {
        build: current,
        build_dir: layout.build_dir(program, current),
        exposes: crate::ops::active_tools(layout, program, current),
        prior_build: Some(target),
        // Honest (this IS the live build we are rolling off) though never read here: only the
        // group transaction's abort discard consults `was_live`, and rollback stages nothing.
        was_live: true,
        reloc: None,
        tree_root: String::new(),
        // The SIGNED index just verified above is the policy, exactly as at install: an
        // ALab program keeps its aliases across the rollback, a vendor extra or a
        // system-satisfiable member grows none — whatever a hand-made `alab-*` link in
        // bin/ might suggest.
        aliases: Aliases::for_program(program, index.program(program)),
    };
    rollback_member(layout, channel, program, &staged);
    Ok(RollbackReport {
        program: program.to_string(),
        from_build: current,
        to_build: target,
        index_build: index.index_build,
        roster_seq: index.roster_seq(),
        coherence_group,
    })
}

/// The transactional `update <grouped-member>` path (§11 tuple-split fix): verify-select the
/// index + freshness like [`apply_channel`], but through `resolve_candidates_live` — the
/// single-program paths take the §14 cache's identity HIT path and none of its failure-time
/// fallback, so a transient index-fetch failure is `NoIndex` rather than a cached index.
/// Then find the ONE coherence group containing
/// `program`, and apply THAT WHOLE group atomically via [`apply_group`]. A grouped member
/// therefore stages-all → flips-all → rolls-back atomically and can NEVER move
/// independently. A program the channel does not pin yields [`FlowError::NotPinned`].
#[allow(
    clippy::too_many_arguments,
    reason = "the transactional single-program update needs the same irreducible inputs as \
              apply_channel plus the program to target: fetcher, layout, root key, channel + \
              triple + program selectors, the installed map, and the floor + clock"
)]
pub fn apply_program(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    channel: &str,
    triple: &str,
    program: &str,
    installed: &BTreeMap<String, u64>,
    excluded: &[String],
    floor: BuildFloor,
    now_unix: i64,
) -> Result<ChannelApplyReport, FlowError> {
    let candidates = resolve_candidates_live(fetcher, layout)?;
    let index = verify_select_fresh(layout, anchor, candidates, floor, now_unix)?;
    // The channel as THIS target sees it (`pin_by_target` laid over `pin`): every decide,
    // plan and fetch below reads this view, never the raw platform-agnostic pin.
    let ch = index
        .channel_for(channel, triple)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    let groups = plan_groups(&index, &ch);
    let group = groups
        .into_iter()
        .find(|g| g.members.iter().any(|m| m == program))
        .ok_or_else(|| FlowError::NotPinned(program.to_string()))?;
    // Dev-linked HARD-SKIP (§13): a tuple with ANY linked member is skipped whole.
    if group
        .members
        .iter()
        .any(|m| crate::linkmode::is_linked(layout, m))
    {
        let skipped_linked = group
            .members
            .iter()
            .filter(|m| crate::linkmode::is_linked(layout, m))
            .cloned()
            .collect();
        return Ok(ChannelApplyReport {
            index_build: index.index_build,
            roster_seq: index.roster_seq(),
            groups: vec![],
            applied: BTreeMap::new(),
            skipped_linked,
            resolved_assets: BTreeMap::new(),
        });
    }
    let mut results = Vec::new();
    let mut applied: BTreeMap<String, AppliedMember> = BTreeMap::new();
    let mut resolved_assets: BTreeMap<String, String> = BTreeMap::new();
    if let Some((acted, o, group_applied)) = apply_group(
        fetcher,
        layout,
        &index,
        &ch,
        channel,
        triple,
        &group,
        installed,
        excluded,
        &mut resolved_assets,
    ) {
        applied.extend(group_applied);
        results.push((acted, o));
    }
    // (Shell.d hook refresh runs at the main.rs CLI edge — see the note in `install`.)
    Ok(ChannelApplyReport {
        index_build: index.index_build,
        roster_seq: index.roster_seq(),
        groups: results,
        applied,
        skipped_linked: vec![],
        resolved_assets,
    })
}

/// Read-only routing decision for the `update` verb (§11): resolve + verify-select the index
/// (verify-before-parse), then return `program`'s coherence group (to pick the transactional-
/// vs-single path) AND the authoritative [`decide`] result (so an ungrouped pin gate can be
/// applied strictly AFTER it, never hiding a Tombstone).
///
/// Read-only as to the STORE; it takes `layout` because the roster ratchet turns on
/// observation ([`verify_select_fresh`]), and this verb is the clearest case for why: it
/// routinely admits a generation and then installs nothing at all (the local-pin hold), and
/// that outcome must still leave the client refusing every older generation afterwards.
#[allow(
    clippy::too_many_arguments,
    reason = "the routing decision needs the fetcher, the layout its roster ratchet turns \
              in, the anchor, the channel + triple + program selectors, the installed build, and \
              the floor + clock the anti-rollback/freshness gates read"
)]
pub fn plan_update(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    channel: &str,
    triple: &str,
    program: &str,
    installed_build: Option<u64>,
    floor: BuildFloor,
    now_unix: i64,
) -> Result<UpdatePlan, FlowError> {
    let candidates = resolve_candidates_live(fetcher, layout)?;
    let index = verify_select_fresh(layout, anchor, candidates, floor, now_unix)?;
    // Decide against the pin THIS target installs (`pin_by_target` overlay), or a build
    // `install` placed from the overlay reads as out of date against another target's pin.
    let ch = index
        .channel_for(channel, triple)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    let ch = &ch;
    let p = index
        .program(program)
        .ok_or_else(|| FlowError::NotReachable(program.to_string(), index_roster(&index)))?;
    Ok(UpdatePlan {
        group: p.coherence_group.clone(),
        decision: decide(ch, program, installed_build),
        // Whether the CURRENTLY-installed build is still gate-valid — a local pin may
        // suppress the ungrouped update only when this is true (never keep a yanked/below-
        // floor build running). See [`crate::gate::current_build_ok`].
        current_build_ok: crate::gate::current_build_ok(ch, program, installed_build),
    })
}

/// The read-only plan `plan_update` yields: which path to take (grouped vs ungrouped), the
/// authoritative [`ApplyDecision`], and whether the currently-installed build is itself still
/// gate-valid (the guard the ungrouped local-pin hold must pass).
#[derive(Debug, Clone)]
pub struct UpdatePlan {
    pub group: Option<String>,
    pub decision: ApplyDecision,
    pub current_build_ok: bool,
}

/// The verified per-build manifest for `program`'s channel pin: pin lookup → repo lookup →
/// fetch → [`TrustedIndex::verify_pkg`] → [`parse_pkg`], in that order (VERIFY-BEFORE-PARSE, §4.2).
/// Returns `(pinned build, repo, manifest)`; `None` on a missing pin/program or any
/// fetch/verify/parse failure. A caller that must bind the signed `program`/`build_number`
/// to its request (anti-replay) checks that on the returned manifest.
///
/// `pub(crate)` for the CLI's pre-install disclosure: the size a user is told before
/// committing gigabytes of disk is summed from these signed manifests'
/// `[cost].disk_installed` (`cli::seed_install_bytes`), so the number on screen comes
/// from the same verified bytes as the install itself rather than a separate estimate.
pub(crate) fn verified_pkg(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    ch: &Channel,
    program: &str,
) -> Option<(u64, String, crate::manifest::PkgManifest)> {
    let pinned = *ch.pin.get(program)?;
    let repo = index.program(program)?.repo.clone();
    let (raw, sig) = fetcher.pkg_manifest(&repo, program, pinned).ok()?;
    let verified = index.verify_pkg(raw, &sig).ok()?;
    let pkg = parse_pkg(&verified).ok()?;
    Some((pinned, repo, pkg))
}

/// The version `program`'s SIGNED manifest states for `build` — any build, not the pin —
/// verified under the index like every manifest read; `None` on any fetch/verify/parse
/// miss. One manifest fetch (a few KB) the landing marker ([`crate::landing`]) spends,
/// for an agent program only and only when a NEWER pin is about to replace `build`, so
/// the wait can say `Ctrl-C runs 2.1.273 now` rather than `build 2026091601`.
fn version_of_build(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    repo: &str,
    program: &str,
    build: u64,
) -> Option<String> {
    let (raw, sig) = fetcher.pkg_manifest(repo, program, build).ok()?;
    let verified = index.verify_pkg(raw, &sig).ok()?;
    let pkg = parse_pkg(&verified).ok()?;
    if !pkg.is_for(program) || pkg.build_number != build {
        return None;
    }
    Some(pkg.version)
}

/// The SIGNED asset size `program`'s pinned build ships for `triple`, or `None` on
/// any fetch/verify/parse miss. The live-progress plan uses it to fix the overall
/// bar's denominator honestly BEFORE bytes move (verify-before-parse preserved via
/// the shared [`verified_pkg`] sequence); a `None` degrades the plan to an
/// unmetered row, never a failure.
pub(crate) fn planned_artifact_size(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    ch: &Channel,
    program: &str,
    triple: &str,
) -> Option<u64> {
    let (_, _, pkg) = verified_pkg(fetcher, index, ch, program)?;
    Some(pkg.artifact_for(triple)?.size)
}

/// Recover the SIGNED `tree_root` for a build that is already installed.
///
/// The attestation is recorded when a member is flipped, and a pass that dies inside
/// the flip window leaves the member LIVE with no row: `atpkg list` shows it, its
/// shims work, and `atpkg verify` fails closed forever with "no signed tree_root
/// recorded; reinstall to enable verification" — while `seed` says "fully installed"
/// and `update` says "up to date", so nothing repairs it. That window is one power
/// loss during a first run (2026-08-20 round-8 audit).
///
/// This re-derives it from the same authority the install used — fetch, verify under
/// the index, bind program and build — so a recovered row is exactly the row the
/// original flip would have written, and never a locally computed guess. `None` on
/// any doubt: an unrecoverable root leaves the fail-closed state untouched.
pub fn signed_root_for_installed(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    program: &str,
    build: u64,
    triple: &str,
) -> Option<String> {
    let repo = index.program(program)?.repo.clone();
    let (raw, sig) = fetcher.pkg_manifest(&repo, program, build).ok()?;
    let verified = index.verify_pkg(raw, &sig).ok()?;
    let pkg = parse_pkg(&verified).ok()?;
    if !pkg.is_for(program) || pkg.build_number != build {
        return None;
    }
    let root = pkg
        .artifacts
        .iter()
        .find(|a| a.target == triple)
        .map(|a| a.tree_root.clone())?;
    (!root.is_empty()).then_some(root)
}

/// The one line a failed group member owes the log: WHICH program, at WHICH build, and WHY
/// it could not be staged.
///
/// [`stage_member`] collapses a dozen distinct failures into `None` so [`transact`] can
/// abort the tuple, and the abort report (`cli::report_channel_apply`, the
/// `TxnOutcome::Aborted` arm) can then say only "ABORTED at <program> during stage — the
/// group stays coherent on its previous builds". That sentence names the member and the
/// phase and NOTHING ELSE, and it is the only surface there is: the store row is the
/// equally mute `aborted: stage`, and `TxnOutcome::Aborted` carries no reason field for
/// anyone to print. On 2026-09-13 five group aborts were logged on one machine and the
/// cause was recoverable from nothing — a signature mismatch, a 403 from the release host,
/// a full disk and a tar-slip refusal all produced byte-identical output.
///
/// The cause IS known at the exits that throw it away: the fetcher hands back its own
/// words, and [`StageError`] renders sha256/tree_root mismatches, extract failures and the
/// tracked-installer refusal — the very strings the SINGLETON lane already shows the user
/// through [`FlowError::Download`] and [`FlowError::Stage`]. The group lane was the only
/// one that dropped them. Printed to stderr so it lands immediately above the abort line in
/// the same log, and deliberately NOT a failure of its own: reporting can never change what
/// the transaction does.
fn stage_failure_note(program: &str, build: u64, why: &str) -> String {
    format!("atpkg: {program} build {build} could not be staged: {why}")
}

/// Stage one group member: fetch + verify + parse its per-build manifest, bind program +
/// build, select the artifact (Shim kinds only — sysroot-bundle fails closed), download, and
/// `verify_and_stage` into its build dir. NO activation. `Some(Staged)` on success (with the
/// prior build captured for rollback); `None` on any failure so [`transact`] aborts the group.
/// Every exit that HOLDS a diagnosis says it first, through [`stage_failure_note`].
#[allow(
    clippy::too_many_arguments,
    reason = "stage_member is the per-member slice of apply_group_txn's irreducible inputs \
              plus the caller's resolved-asset collector the pass-end gc sparing reads"
)]
fn stage_member(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    ch: &Channel,
    program: &str,
    triple: &str,
    prior_build: Option<u64>,
    resolved_assets: &mut BTreeMap<String, String>,
    why: &mut String,
) -> Option<Staged> {
    // `why` is the sentence the abort carries (2026-09-15): every `None` below names its
    // step, so `aborted: stage` on the record is never the whole story again.
    let Some((pinned, repo, pkg)) = verified_pkg(fetcher, index, ch, program) else {
        *why = format!("the signed manifest for {program} could not be fetched or verified");
        return None;
    };
    if !pkg.is_for(program) || pkg.build_number != pinned {
        *why = format!(
            "the manifest fetched for {program} is not the build the index pins ({pinned})"
        );
        return None;
    }
    let Some(artifact) = pkg.artifact_for(triple) else {
        *why = format!("{program} build {pinned} publishes no artifact for {triple}");
        return None;
    };
    // The pass's verified answer for this member, recorded BEFORE the download so a
    // member that fails mid-transfer still reaches the report — the pass-end
    // `gc::run_keeping_pinned_partials` spares exactly these programs' `<asset>.part`
    // resume files, and the failed member is the one the sparing exists for.
    resolved_assets.insert(program.to_string(), artifact.asset.clone());
    // Same admission as `install`: a row that fails `check_row` aborts the group before
    // any byte moves.
    if let Err(e) = crate::vendor::check_row(artifact, &pkg.exposes) {
        *why = format!("the signed row for {program} was refused: {e}");
        return None;
    }
    // Shim, vendor-app and sysroot-bundle members stage on this path; the aterm
    // app-bundle, the OS-installer protocols (nothing of theirs is staged, and a
    // coherence group is a STORE transaction), the not-yet-built `system-pm` and unknown
    // pairs fail closed (return None → the group aborts), exactly like `install`.
    let strategy = crate::dispatch::strategy_for(&artifact.kind, &artifact.protocol);
    let reloc = match strategy {
        crate::dispatch::ApplyStrategy::Shim | crate::dispatch::ApplyStrategy::VendorApp => None,
        crate::dispatch::ApplyStrategy::SysrootBundle => Some(artifact.reloc.clone()),
        crate::dispatch::ApplyStrategy::AppBundle
        | crate::dispatch::ApplyStrategy::Pkg
        | crate::dispatch::ApplyStrategy::SoftwareUpdate
        | crate::dispatch::ApplyStrategy::SystemPm
        | crate::dispatch::ApplyStrategy::Unknown => {
            // A kind/protocol pair this lane cannot stage is a PUBLISHING fact, not a
            // machine fault, and it aborts a whole tuple — so name it rather than leaving
            // the operator to guess at "ABORTED at <program> during stage".
            *why = format!(
                "{program}'s row ({}/{}) is not one a coherence group stages",
                artifact.kind, artifact.protocol
            );
            eprintln!(
                "{}",
                stage_failure_note(
                    program,
                    pinned,
                    "its kind/protocol pair has no staging lane in this client",
                )
            );
            return None;
        }
    };
    // The same digest-refusal memo the singleton lane consults, in the lane that meets
    // it on the fleet: a tuple whose biggest member is pinned to digests the published
    // asset does not match re-downloaded that member every six hours. The abort is the
    // same one the stage failure would have produced — minus the transfer.
    if let Some(note) = digest_refusal_note(layout, program, pinned, artifact) {
        eprintln!("{}", stage_failure_note(program, pinned, &note));
        *why = note;
        return None;
    }
    let dl = match staged_download_path(layout, program, &artifact.asset) {
        Ok(dl) => dl,
        Err(e) => {
            *why = format!("no staging path for {program}: {e}");
            return None;
        }
    };
    if let Some(parent) = dl.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        *why = format!("cannot create {}: {e}", parent.display());
        return None;
    }
    // Live-progress hooks, mirroring the singleton path (no-ops without a live pass).
    crate::progress::note_build(program, pinned);
    // THE CARRIED ARCHIVE ([`carried_archive`]). An earlier pass of this tuple can have
    // aborted on some OTHER member after this one downloaded, verified and staged: that
    // abort discarded this member's extracted TREE (it must — see `apply_group_txn`), but
    // the archive the tree came out of is the signed artifact's own bytes, and re-using it
    // is what bounds the retry to the members that actually failed. The same archive is
    // what THIS member's own failed stage leaves behind when the failure was not the
    // bytes' ([`reclaim_after_failed_stage`], 2026-09-15) — the provenance refusal of
    // 2026-09-14 re-downloaded 743 MB of `trust` at every launch and six-hour tick.
    let carried = carried_archive(&dl, artifact);
    if !carried {
        // Same reason as the singleton path: a stale staging entry can be a hardlink
        // into the sealed registry, and fetching over it corrupts the app bundle. The
        // carried arm never needs this: it only READS `dl` — nothing is written over a
        // hardlink — and what it reads already matched the signed digest.
        let _ = std::fs::remove_file(&dl);
    }
    // Same strandage bound as the singleton path: at most one partial — and one
    // carried archive — per program survives between passes, and it is the one the
    // next attempt will finish or reuse.
    sweep_foreign_partials(&dl);
    if carried {
        println!(
            "atpkg: {program}: reusing the verified archive the previous attempt left \
             ({})",
            artifact.asset
        );
    } else {
        let download_watch = crate::progress::watch_download(program, &dl, artifact.size);
        if let Err(e) = fetch_artifact(fetcher, program, &repo, artifact, &dl) {
            // SAY WHY (2026-09-13; see `stage_failure_note`). The transfer is the failure
            // this exit meets most often and `e` carries the fetcher's own words — HTTP
            // status, curl exit, the signed byte cap — which no later surface can
            // reconstruct, because the abort report only ever learns the member's NAME.
            eprintln!("{}", stage_failure_note(program, pinned, &e));
            // An aborted transfer leaves bytes in `<dl>.part`, not at `<dl>`: the production
            // fetcher promotes the part onto `dl` only on curl success, so `dl` is either
            // absent or complete. The part is deliberately LEFT for the next attempt to
            // continue from (`aterm_update_core::download_to_resumable`); `dl` itself is
            // reclaimed on this exit exactly as on the stage exit below, because a `dir:`
            // registry's copy lane writes there directly.
            let _ = std::fs::remove_file(&dl);
            *why = format!("the download of {} failed: {e}", artifact.asset);
            return None;
        }
        // The transfer is over — stop the poller before the phase moves on.
        drop(download_watch);
    }
    crate::progress::note_phase(program, crate::progress::Phase::Verify);
    let build_dir = layout.build_dir(program, pinned);
    // Capture "this build is ALREADY live" BEFORE the stage swaps a new tree into it, and
    // before any flip can move the links: on a flip-phase abort `flip_member`/`rollback_member`
    // re-point or REMOVE the very links this asks about, so by discard time the answer is
    // gone. `installed` is the shim view and can be silent for a live build (see the abort
    // discard in `apply_group_txn`), which is the whole reason this flag exists.
    let was_live = std::fs::read_link(layout.program_current(program))
        .is_ok_and(|t| t == build_dir)
        || std::fs::read_link(layout.channel_current(&ch.name)).is_ok_and(|t| t == build_dir);
    // Reclaim the compressed asset on every FAILING exit: a group member that fails to
    // stage otherwise strands its archive in `staging/` forever, and nothing else ever
    // sweeps that directory (`gc::interrupted_debris` walks `store/` only). The HAPPY exit
    // keeps it instead, and `apply_group_txn` reclaims it once the TUPLE's fate is known —
    // so a sibling's failure costs this member its tree but never its download.
    // The signed `shim_env` rides beside the build (design S7) so the flip — and a
    // rollback that has no manifest in hand — lays this build's shims with it.
    if let Err(e) = crate::shim_env::write_sidecar(&build_dir, &pkg.shim_env()) {
        *why = format!("the shim_env sidecar for {program} could not be written: {e}");
        // A store the process cannot write is the likeliest reading of this exit, and it
        // aborts the tuple exactly like a bad signature would — so the log must be able to
        // tell the two apart (2026-09-13).
        eprintln!(
            "{}",
            stage_failure_note(
                program,
                pinned,
                "the signed shim_env sidecar could not be written beside the build",
            )
        );
        let _ = std::fs::remove_file(&dl);
        *why = format!("the shim_env sidecar for {program} could not be written: {e}");
        return None;
    }
    let extract_scope = crate::progress::extract_scope(program, artifact.cost.disk_installed);
    let staged = verify_and_stage(artifact, &dl, &build_dir);
    drop(extract_scope);
    if let Err(e) = staged {
        // Reclaimed on THIS exit for a DIGEST failure only (the bytes are wrong — the
        // archive and any `.part` that would seed the next attempt go); every other
        // failure keeps the verified archive for the next attempt to stage without a
        // download ([`reclaim_after_failed_stage`], 2026-09-15). A member that STAGED keeps
        // its archive until the transaction knows the whole tuple's fate.
        record_digest_refusal(&build_dir, artifact, &e);
        reclaim_after_failed_stage(&dl, &e);
        // SAY WHY (2026-09-13; see `stage_failure_note`). `StageError`'s Display is the
        // same text the singleton lane already shows through `FlowError::Stage` — a
        // sha256 or tree_root mismatch with both digests, a tar-slip/size-cap refusal,
        // the tracked-installer remedy — and it is the ONE sentence that separates "the
        // publisher shipped a bad asset" from "this machine could not unpack it".
        eprintln!("{}", stage_failure_note(program, pinned, &e.to_string()));
        *why = format!("staging {} failed: {e}", artifact.asset);
        return None;
    }
    // This member's bytes are good, whatever an earlier pass recorded about them (the
    // singleton lane's reasoning, member by member).
    crate::store::clear_stage_refusal(&build_dir);
    // The archive of a member that STAGED stays until the transaction knows the whole
    // tuple's fate (`carried_archive`): a sibling's abort discards this tree, and the
    // archive is what spares the retry the download.
    Some(Staged {
        build: pinned,
        build_dir,
        // Refused (sensitive/malformed) names are dropped here rather than at flip time —
        // same outcome as before, one admission instead of one per flip/rollback pass. A
        // group member's refusals are not a stage failure, matching `install`.
        exposes: crate::store::split_exposed(&pkg.exposes).0,
        prior_build,
        was_live,
        reloc,
        tree_root: artifact.tree_root.clone(),
        aliases: Aliases::for_program(program, index.program(program)),
    })
}

/// Flip a staged member live: point the channel `current` at its new build and (re)install
/// its shims. `true` on success. A partial flip (shims IO error after `current` was already
/// re-pointed) is self-undone via [`rollback_member`] so a `false` return leaves NO live
/// pointer into the new build — the abort cleanup then discards it. (Shim refusals for
/// sensitive names are not a flip failure; they are honestly dropped, matching `install`.)
fn flip_member(layout: &Layout, channel: &str, program: &str, s: &Staged) -> bool {
    // A sysroot-bundle member gets its pre-activation wiring before the flip; a
    // failure here aborts the group (the payload is staged but never activated).
    if let Some(reloc) = &s.reloc
        && apply_sysroot_bundle(reloc).is_err()
    {
        return false;
    }
    if activate_channel(layout, channel, &s.build_dir).is_err() {
        // Each LINK is atomic, but `activate_channel` writes two of them and the
        // per-program witness goes first: a failure in the channel half leaves
        // `store/<program>/current` already naming the build the abort cleanup is
        // about to delete. Point the witness back at the prior build (fresh
        // install: remove it) so it keeps agreeing with the shims, which were
        // never touched. If the failure was in the witness half instead, both
        // repairs are no-ops — re-pointing writes what was already there, and
        // removing a link that does not exist does nothing.
        match s.prior_build {
            Some(prior) => {
                let _ = crate::activate::atomic_symlink(
                    &layout.build_dir(program, prior),
                    &layout.program_current(program),
                );
            }
            None => crate::platform::remove_link(&layout.program_current(program)),
        }
        return false;
    }
    // The build's own `shim_env` (its sidecar, written from the signed manifest when
    // it was staged) rides on every shim the flip lays — design S7.
    let env = crate::shim_env::read_sidecar(&s.build_dir);
    if install_tools_env(layout, &s.build_dir, &s.exposes, s.aliases, &env).is_err() {
        rollback_member(layout, channel, program, s);
        return false;
    }
    // Fail-loud resolve check for a bundle: a broken toolchain rolls the flip back.
    if s.reloc.is_some() && bundle_resolve_check(&s.build_dir, &s.exposes).is_err() {
        rollback_member(layout, channel, program, s);
        return false;
    }
    true
}

/// `staging/<program>/<asset>` — where a byte-moving row downloads to — or
/// [`FlowError::VendorRefused`] when `asset` is not one plain file name. Both callers
/// unlink this path and sweep every `*.part` beside it BEFORE any download, and
/// `vendor::check_row` holds `https` rows to a bare name but not `github-release` ones:
/// an absolute asset (`Path::join` replaces the base) or a `..` climb would have a signed
/// row delete files anywhere the user can write (audit K3, 2026-09-12). Checked on the
/// path itself, so no protocol lane can reach those deletions with one that escapes.
fn staged_download_path(layout: &Layout, program: &str, asset: &str) -> Result<PathBuf, FlowError> {
    let mut comps = Path::new(asset).components();
    let one_name = matches!(
        (comps.next(), comps.next()),
        (Some(std::path::Component::Normal(_)), None)
    );
    if !one_name || asset.contains('/') || asset.contains('\\') {
        let mut why = String::from(
            "asset must be a bare file name inside staging (no separators, not `.`/`..`): ",
        );
        why.push_str(asset);
        return Err(FlowError::VendorRefused(why));
    }
    Ok(layout.staging_dir(program).join(asset))
}

/// Whether `build` of `program` is a build a rollback may actually LAND ON: its `bin/` still
/// holds at least one file [`rollback_member`] could re-point a shim at.
///
/// **THE GATE THAT WAS MISSING WHEN A CORPSE WAS THE ROLLBACK TARGET.** Selection above
/// reads [`crate::ops::list_installed`], which counts a build as installed on the strength
/// of its `<n>.ready` marker. A marker is a file; the tools are the tree; and on m3 the two
/// disagreed — `store/trust/8595/` with no `bin/` at all, 417 MB of orphaned `lib/`, and
/// `8595.ready` beside it still saying `ok` (2026-09-17). Selected, that build reaches
/// [`rollback_member`], whose restore loop asks `target.exists()` of every tool and takes
/// the `else` arm — `remove_file(layout.shim(tool))` — for every one of them. The verb would
/// have reported a successful rollback and left the machine with no compiler, no verifier
/// and no `ay`: a disarm, reported as a success.
///
/// Two fixes stand behind this one and neither makes it redundant. The corpse can no longer
/// be MADE ([`crate::store::discard_build`], [`crate::ops::uninstall`]: the marker comes
/// down before the tree goes), and a marker written from now on RECORDS its `bin/` and is
/// refuted by the disk when that `bin/` is gone (the `contents=` record
/// [`crate::store::build_is_complete`] checks). This is the clause that also covers the builds
/// ALREADY on disk, whose markers were written by a version that recorded nothing, and every
/// way a tree can lose its contents that neither of those reaches — a person's interrupted
/// `rm -rf`, a restored backup, a volume that dropped a directory.
///
/// The predicate is deliberately weak: ONE surviving entry is enough. It is not this
/// function's job to decide that a build is complete — [`crate::store::build_is_complete`]
/// owns that question and has the marker to reason from — only to refuse the one shape that
/// turns a rollback into a disarm, and to refuse it without ever declining a build whose
/// `bin/` this process merely cannot read: `read_dir` failing for any reason other than
/// `NotFound` is a bound on what we may know, and answers `true`.
fn build_can_be_rolled_onto(layout: &Layout, program: &str, build: u64) -> bool {
    let bin = layout.build_dir(program, build).join("bin");
    match std::fs::read_dir(&bin) {
        Ok(entries) => entries.flatten().next().is_some(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        // EACCES on a shared prefix, macOS privacy consent, EIO: nobody looked, so nothing
        // is refuted. A rollback onto such a build behaves exactly as it did before.
        Err(_) => true,
    }
}

/// Roll a flipped (or partially-flipped) member back to the build it pointed at before this
/// transaction — its `prior_build`. For a tool the prior build actually contains, the shim
/// is re-pointed there; a tool the NEW build ADDED but the prior lacks has its shim REMOVED
/// (re-pointing it would dangle). A fresh install (`prior_build == None`) removes the shims.
///
/// The "does the prior build contain it?" probe must name the EXECUTABLE
/// ([`ToolName::exe_file`]), never the bare tool name — see [`ToolName`]'s docs.
fn rollback_member(layout: &Layout, channel: &str, program: &str, s: &Staged) {
    match s.prior_build {
        Some(prior) if prior != s.build => {
            let prior_dir = layout.build_dir(program, prior);
            // The restore set is the UNION of the new build's exposes and the prior
            // build's actual `bin/` contents. `s.exposes` alone misses a tool the
            // prior build shipped and the new one DROPPED: the new build's shim
            // prune (`install_tools`) already deleted that tool's shim — same
            // program, different build, exactly its job — so restoring only the new
            // list would re-activate the prior build with part of its PATH surface
            // silently missing. A sensitive name in the prior `bin/` never had a
            // shim (`ToolName::new` refuses it) and drops out here the same way.
            let mut restore: std::collections::BTreeSet<crate::store::ToolName> =
                s.exposes.iter().cloned().collect();
            if let Ok(entries) = std::fs::read_dir(prior_dir.join("bin")) {
                for e in entries.flatten() {
                    // Files only: a stray subdirectory in `bin/` is not a tool and
                    // must not grow a shim.
                    if !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                        continue;
                    }
                    if let Some(name) = e.file_name().to_str() {
                        // Total strip, same semantics as `ToolName::from_shim_file`:
                        // on Unix `EXE_SUFFIX` is `""` and this is the identity.
                        let logical = name
                            .strip_suffix(crate::platform::EXE_SUFFIX)
                            .unwrap_or(name);
                        if let Some(tool) = crate::store::ToolName::new(logical) {
                            restore.insert(tool);
                        }
                    }
                }
            }
            // The PRIOR build's shims export the PRIOR build's `shim_env` — its sidecar,
            // written from its own signed manifest when it was staged (design S7). A
            // build staged before the sidecar existed has none, and reads as none.
            let env = crate::shim_env::read_sidecar(&prior_dir);
            // THE PRIOR BUILD'S EXEC ROOT BEFORE ITS SHIMS (`crate::compat`). This lay
            // renders through `platform::install_shim_env`, not `install_tools_env`, so the
            // root that function ensures was never laid here: a rollback to trust 8590 —
            // `atpkg rollback`, a flip whose resolve check fails, a group abort — re-pointed
            // `tippy` at a build whose own tippy refuses its store path, with no root for
            // the render to route through (a reviewer measured the plain shim, 2026-09-15),
            // and the single-program update and install lanes run no reconcile after it.
            // Best-effort like the rest of an undo: a root that cannot be laid leaves the
            // shims plain, running the store path as before, and says so.
            if let Some(n) = crate::compat::trust_build_of(layout, &prior_dir) {
                match crate::compat::ensure_root(layout, &prior_dir, crate::seam::Depth::Shallow) {
                    Ok(crate::compat::Ensured::Built) => {
                        println!("{}", crate::compat::laid_line(layout, n));
                    }
                    Ok(crate::compat::Ensured::Plain | crate::compat::Ensured::Present) => {}
                    Err(e) => eprintln!("{}", crate::compat::not_laid_line(layout, n, &e)),
                }
            }
            for tool in &restore {
                let target = prior_dir.join("bin").join(tool.exe_file());
                // The alias goes where the primary goes: re-pointed at the prior build
                // beside it, or dropped with it. `None` when the policy is Off (the alias
                // was never laid) or the tool is its own alias.
                let alias = (s.aliases == Aliases::Alab).then(|| tool.alias()).flatten();
                if target.exists() {
                    let _ = crate::platform::install_shim_env(
                        &prior_dir.join("bin"),
                        tool,
                        &layout.shim(tool),
                        &env,
                    );
                    if let Some(alias) = &alias {
                        let _ = crate::platform::install_shim_env(
                            &prior_dir.join("bin"),
                            tool,
                            &layout.shim(alias),
                            &env,
                        );
                    }
                } else {
                    // A binary the new build added but the prior build lacks — drop the shim
                    // rather than leave it dangling at a nonexistent prior-build path.
                    let _ = std::fs::remove_file(layout.shim(tool));
                    if let Some(alias) = &alias {
                        let _ = std::fs::remove_file(layout.shim(alias));
                    }
                }
            }
            let _ = activate_channel(layout, channel, &prior_dir);
        }
        Some(_) => { /* prior == new build: nothing meaningful to undo */ }
        None => {
            for tool in &s.exposes {
                let _ = std::fs::remove_file(layout.shim(tool));
                if s.aliases == Aliases::Alab
                    && let Some(alias) = tool.alias()
                {
                    let _ = std::fs::remove_file(layout.shim(&alias));
                }
            }
            // A fresh install has no prior link to re-point, but `flip_member`'s
            // `activate_channel` DID write `store/<program>/current` — and the abort
            // cleanup is about to delete the build it names. Remove the link too
            // (platform::remove_link — on Windows it is a junction that remove_file
            // refuses), or the program is left with a permanently dangling witness
            // link: harmless to GC (a broken own link means abstain), but a state no
            // ordinary operation should ever leave behind.
            crate::platform::remove_link(&layout.program_current(program));
        }
    }
    // THE FRONT-OF-PATH TWIN FOLLOWS ITS PRIMARY (owner decision 2026-09-10). This undo
    // re-points — or removes — `bin/<tool>` through `platform::install_shim_env`, never
    // `install_tools_env`, so the reconcile that lay ends with had never run here: every
    // exit of this function (`atpkg rollback`, a failed flip, a group abort, an aborted
    // fresh install) left `agents/<agent>` still exec'ing the build it just moved OFF, and
    // `agents/` is FIRST on every PATH — so typing `claude` kept running the build the
    // user rolled away from until some later update or repair pass reconciled, and once
    // gc reclaimed it (its claim union reads `bin/` shims and `current` links, never
    // `agents/`) that front-of-PATH name exec'd a deleted file. Placed after the match so
    // it covers the fresh-install arm too, where the sweep inside takes a twin whose
    // primary has just gone. Best-effort and idempotent, like the rest of an undo.
    crate::activate::reconcile_agents(layout);
}

/// Sysroot-bundle pre-activation wiring, dispatched on the signed `reloc` policy
/// (§10.1). `self-contained` (the pack-time-relocated default) needs nothing — the
/// payload already carries its dependencies, which is the only policy the trust
/// toolchain ships. Every other policy fails closed.
fn apply_sysroot_bundle(reloc: &str) -> Result<(), FlowError> {
    match reloc {
        "self-contained" => Ok(()),
        other => Err(FlowError::UnsupportedKind(format!("reloc={other}"))),
    }
}

/// Run the fail-loud [`crate::sysroot::resolve_check`] over every exposed binary a
/// bundle ships, so a toolchain whose dynamic loader can't resolve its libraries
/// aborts the apply instead of being reported as a successful install.
///
/// Takes the ADMITTED tool set: a name refused a shim is not reachable from `PATH`, so
/// whether its loader resolves is not a property of the install. It also names the binary via
/// [`ToolName::exe_file`] rather than joining the bare tool — the same omission
/// `rollback_member` had, harmless in practice (a PE starts with `MZ`, which fails
/// [`crate::relocate::is_native_object`], and the bundle backend errors on Windows anyway) but
/// not worth leaving as the one site that still spells the rule by hand.
fn bundle_resolve_check(build_dir: &Path, exposes: &[ToolName]) -> Result<(), FlowError> {
    for tool in exposes {
        let bin = build_dir.join("bin").join(tool.exe_file());
        // Only native objects (the compilers that link dylibs) have libraries to
        // resolve; a wrapper script has none, so skip it.
        if bin.is_file() && crate::relocate::is_native_object(&bin) {
            crate::sysroot::resolve_check(&bin).map_err(FlowError::Activate)?;
        }
    }
    Ok(())
}

/// Current Unix epoch second — the `now_unix` input every freshness gate takes as a
/// parameter (the flow entry points stay clock-free for determinism; the CLI edge reads
/// the real clock through THIS one definition).
///
/// # A clock we cannot read fails CLOSED — `i64::MAX`, never `0`
///
/// This used to return `0` on a pre-epoch clock, and that was safe only while this value
/// fed one index's freshness window. It now also drives the ROSTER's `valid_until` and every
/// machine's `not_after`, and zero reads as 1970 — before every conceivable deadline — so a
/// roster generation that lapsed years ago would be ADMITTED and an expired machine would be
/// treated as live. Roster freshness is the only defence a fresh install has (it carries no
/// floor), so that is the one gate that must not fail open.
///
/// `i64::MAX` makes every window look already-expired, which refuses everything. It is the
/// same choice, for the same reason, as `aterm-update`'s `github::unix_now`, and the
/// opposite of a retry-deadline clock (where "passed" means "retry now" and zero is right).
/// The direction is asserted in this module's tests.
pub(crate) fn now_unix() -> i64 {
    unix_or_fail_closed(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH))
}

/// [`now_unix`]'s decision, separated from the clock read so the fail-closed direction is
/// TESTABLE rather than asserted in prose. Takes exactly what `duration_since` returns; the
/// tests feed it a genuine `SystemTimeError` (`UNIX_EPOCH.duration_since(now())`, which
/// errors on any post-1970 clock) rather than a stand-in.
fn unix_or_fail_closed(
    since_epoch: Result<std::time::Duration, std::time::SystemTimeError>,
) -> i64 {
    since_epoch.map_or(i64::MAX, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Parse an RFC3339 UTC timestamp `YYYY-MM-DDTHH:MM:SSZ` to a Unix epoch second. Pure (no
/// clock), so the freshness gate stays deterministic; `None` on any malformed field, which
/// the caller treats as lapsed (fail closed). The `Z` suffix is REQUIRED and the length
/// exact: a timezone-offset stamp (`…+09:00`) must not be silently read as UTC — up to 14h
/// of fail-open skew on the freshness gate — and trailing bytes past the seconds field must
/// not parse at all; both are refused so the producer contract (`tools/atpkg-*.sh` and
/// `now_rfc3339` both emit exactly this shape) is enforced instead of assumed. Calendar
/// math is the shared `aterm_types::rfc3339::days_from_civil`.
pub(crate) fn rfc3339_to_unix(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let mo: i64 = s.get(5..7)?.parse().ok()?;
    let d: i64 = s.get(8..10)?.parse().ok()?;
    let h: i64 = s.get(11..13)?.parse().ok()?;
    let mi: i64 = s.get(14..16)?.parse().ok()?;
    let se: i64 = s.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let days = aterm_types::rfc3339::days_from_civil(y, mo, d);
    Some(days * 86400 + h * 3600 + mi * 60 + se)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use std::collections::HashMap;
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::rc::Rc;

    use crate::sig::testkit;

    /// The synthetic paper master, and the one machine its roster authorizes. The whole
    /// crate signs with this same pair, so a flow test cannot prove something under a
    /// trust shape no other layer uses. (`ROOT_SEED` still names the ROOT of trust — it
    /// is just the paper master now, not a package-specific root.)
    const ROOT_SEED: [u8; 32] = testkit::MASTER_SEED;
    const RELEASE_SEED: [u8; 32] = testkit::MACHINE_SEED;

    /// A group member's stage failure must reach the log as a sentence that says WHY. The
    /// abort report can only print "ABORTED at <program> during stage" — on 2026-09-13 five
    /// aborts were logged that way and no surface anywhere recorded a cause — so the note
    /// `stage_member` prints at each failing exit has to carry all three facts: the member,
    /// the build it was moving to, and the underlying error's OWN words (here `StageError`'s
    /// Display, the same text the singleton lane shows through `FlowError::Stage`).
    #[test]
    fn stage_failure_note_names_program_build_and_cause() {
        let cause = StageError::Sha256Mismatch {
            expected: "aa".to_string(),
            got: "bb".to_string(),
        }
        .to_string();
        let note = stage_failure_note("trust", 4210, &cause);
        assert!(
            note.contains("trust"),
            "the note must name the member the group aborted at, got: {note}"
        );
        assert!(
            note.contains("4210"),
            "the note must name the build being staged, got: {note}"
        );
        assert!(
            note.contains("asset sha256 mismatch") && note.contains("bb"),
            "the note must carry the stage error's own words, got: {note}"
        );
    }

    /// A durable build floor of `index_build`, recorded under the generation these fixtures
    /// publish at — i.e. a floor that actually BINDS. Written as a helper rather than a bare
    /// integer because `BuildFloor` carries the generation that set it: a floor stamped with
    /// some OTHER generation is waived, and a floor test that accidentally built one would
    /// pass vacuously. `fl(0)` is "nothing recorded", which admits everything.
    fn fl(index_build: u64) -> BuildFloor {
        BuildFloor {
            index_build,
            roster_seq: testkit::SEQ,
        }
    }

    /// The anchor every flow test resolves under: armed with the synthetic master, roster
    /// floor 0. `anchor_of` is the seam the negative tests use to arm a DIFFERENT master.
    fn anchor() -> Anchor {
        anchor_of(&ROOT_SEED)
    }

    fn anchor_of(master_seed: &[u8; 32]) -> Anchor {
        Anchor::of(vec![pk(master_seed)], 0)
    }

    /// The attribution head every fixture index carries — `machine_id` + `roster_seq`
    /// must name the machine that actually signs, or the bind refuses the index.
    fn attribution() -> String {
        format!(
            "machine_id = \"{}\"\nroster_seq = {}\n",
            testkit::MACHINE_ID,
            testkit::SEQ
        )
    }
    const TRIPLE: &str = "aarch64-apple-darwin";

    fn kp(seed: &[u8; 32]) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(seed).unwrap()
    }
    fn pk(seed: &[u8; 32]) -> String {
        aterm_codec::base64::encode(kp(seed).public_key().as_ref()).expect("32-byte key")
    }
    fn sign(seed: &[u8; 32], msg: &[u8]) -> Vec<u8> {
        kp(seed).sign(msg).as_ref().to_vec()
    }

    fn scratch(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("atpkg-flow-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        d
    }

    /// A raw USTAR + zstd archive with `bin/ay` (+ a sensitive `bin/git` to prove the shim
    /// gate). Returns the path.
    fn make_archive(dir: &Path) -> PathBuf {
        make_archive_with(dir, b"#!/bin/true\nay", b"0000755\0")
    }

    /// As [`make_archive`], but with explicit `bin/ay` content + tar mode (the
    /// resolve-check rollback tests ship a native-object magic with NO exec bit, so the
    /// spawn fails and the fail-loud check takes its spawn-failure arm).
    fn make_archive_with(dir: &Path, ay_content: &[u8], ay_mode: &[u8; 8]) -> PathBuf {
        fn entry(name: &str, content: &[u8], mode: &[u8; 8]) -> Vec<u8> {
            let mut h = [0u8; 512];
            let nb = name.as_bytes();
            h[..nb.len()].copy_from_slice(nb);
            h[100..108].copy_from_slice(mode);
            h[108..116].copy_from_slice(b"0000000\0");
            h[116..124].copy_from_slice(b"0000000\0");
            h[124..136].copy_from_slice(format!("{:011o}\0", content.len()).as_bytes());
            h[136..148].copy_from_slice(b"00000000000\0");
            h[148..156].copy_from_slice(b"        ");
            h[156] = b'0';
            h[257..263].copy_from_slice(b"ustar\0");
            h[263..265].copy_from_slice(b"00");
            let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
            h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
            let mut out = h.to_vec();
            out.extend_from_slice(content);
            out.resize(out.len() + (512 - content.len() % 512) % 512, 0);
            out
        }
        let mut tar = Vec::new();
        tar.extend(entry("bin/ay", ay_content, ay_mode));
        tar.extend(entry(
            "bin/git",
            b"#!/bin/true\nnot-really-git",
            b"0000755\0",
        )); // sensitive → refused shim
        tar.resize(tar.len() + 1024, 0);
        let path = dir.join("ay-18.tar.zst");
        let f = std::fs::File::create(&path).unwrap();
        let mut enc = zstd::Encoder::new(f, 0).unwrap();
        enc.write_all(&tar).unwrap();
        enc.finish().unwrap();
        path
    }

    /// A fake fetcher serving a fixed signed index, a fixed signed pkg manifest, and a
    /// local archive (copied on download).
    struct Fake {
        index: Vec<u8>,
        index_sig: Vec<u8>,
        pkg: HashMap<(String, u64), (Vec<u8>, Vec<u8>)>,
        archives: HashMap<String, PathBuf>,
    }
    impl Fetcher for Fake {
        fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
            // The master-signed roster is published WITH the index, exactly as a real
            // release carries `aterm-machines.toml` beside `index.toml`.
            let (roster_bytes, roster_sig) = testkit::published_roster();
            Ok(vec![Candidate {
                label: "v0".into(),
                index_bytes: self.index.clone(),
                sig: self.index_sig.clone(),
                roster_bytes,
                roster_sig,
            }])
        }
        fn pkg_manifest(
            &self,
            _repo: &str,
            program: &str,
            build: u64,
        ) -> Result<(Vec<u8>, Vec<u8>), String> {
            self.pkg
                .get(&(program.to_string(), build))
                .cloned()
                .ok_or_else(|| "no such manifest".into())
        }
        fn download(&self, _repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
            let src = self.archives.get(asset).ok_or("no such asset")?;
            std::fs::copy(src, dest)
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
    }

    /// Build the whole synthetic signed release (index + pkg + archive) consistent with the
    /// real sha256 + tree_root of the archive.
    fn fixture(dir: &Path) -> Fake {
        fixture_with_kind(dir, "binary")
    }

    /// As [`fixture`], but with an explicit artifact `kind` (to exercise the
    /// per-member dispatch — e.g. `sysroot-bundle`, which must fail closed).
    fn fixture_with_kind(dir: &Path, kind: &str) -> Fake {
        fixture_from(dir, kind, make_archive(dir), None)
    }

    /// As [`fixture_with_kind`], but with explicit `bin/ay` content + tar mode (the
    /// resolve-check rollback tests ship a native-object magic that cannot spawn).
    fn fixture_with(dir: &Path, kind: &str, ay_content: &[u8], ay_mode: &[u8; 8]) -> Fake {
        fixture_from(dir, kind, make_archive_with(dir, ay_content, ay_mode), None)
    }

    /// The signed release over an archive the caller already built.
    ///
    /// `signed_root` overrides the `tree_root` the manifest carries. `None` is the honest
    /// case — the real root of the real archive. `Some(..)` is how a test makes the SIGNED
    /// value disagree with the bytes on disk, which is the only way to reach the apply-time
    /// TOCTOU re-verify through the real flow: the artifact's `sha256` covers the compressed
    /// asset, so no substituted archive can satisfy that gate and still fail a later one.
    fn fixture_from(dir: &Path, kind: &str, archive: PathBuf, signed_root: Option<&str>) -> Fake {
        let sha = crate::tree::file_sha256(&archive).unwrap();
        // Learn the extracted tree_root by a throwaway stage.
        let probe = dir.join("probe");
        let _ = std::fs::remove_dir_all(&probe);
        crate::extract::extract_tar_zst(&archive, &probe, 10_000_000, 10_000).unwrap();
        let root = signed_root.map_or_else(
            || crate::tree::tree_root(&probe).unwrap(),
            std::string::ToString::to_string,
        );

        let index_body = format!(
            "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
             [programs.ay]\nrepo = \"ay\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             pin = {{ ay = 18 }}\n",
            attr = attribution()
        );
        let pkg_body = format!(
            "schema = 2\nprogram = \"ay\"\nversion = \"0.1\"\nbuild_number = 18\n\
             exposes = [\"ay\", \"git\"]\n\
             [[artifact]]\ntarget = \"{TRIPLE}\"\nkind = \"{kind}\"\nasset = \"ay-18.tar.zst\"\n\
             sha256 = \"{sha}\"\ntree_root = \"{root}\"\nsize = 100\n\
             [artifact.cost]\ndisk_installed = 1048576\n"
        );
        let mut pkg = HashMap::new();
        pkg.insert(
            ("ay".to_string(), 18u64),
            (
                pkg_body.clone().into_bytes(),
                sign(&RELEASE_SEED, pkg_body.as_bytes()),
            ),
        );
        let mut archives = HashMap::new();
        archives.insert("ay-18.tar.zst".to_string(), archive);
        Fake {
            index: index_body.clone().into_bytes(),
            index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
            pkg,
            archives,
        }
    }

    /// A signed release whose ONE artifact row carries the archive's REAL digests
    /// (sha256/tree_root/size) and, appended verbatim, `row` — the `kind`/`protocol` pair
    /// and the vendor keys under test — so a row that passes admission can stage end to
    /// end through a fetcher that serves the archive on `download_url`.
    fn fixture_vendor(dir: &Path, row: &str) -> Fake {
        let archive = make_archive(dir);
        let sha = crate::tree::file_sha256(&archive).unwrap();
        let probe = dir.join("probe");
        let _ = std::fs::remove_dir_all(&probe);
        crate::extract::extract_tar_zst(&archive, &probe, 10_000_000, 10_000).unwrap();
        let root = crate::tree::tree_root(&probe).unwrap();
        let size = std::fs::metadata(&archive).unwrap().len();
        let index_body = format!(
            "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
             [programs.ay]\nrepo = \"ay\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             pin = {{ ay = 18 }}\n",
            attr = attribution()
        );
        let pkg_body = format!(
            "schema = 2\nprogram = \"ay\"\nversion = \"0.1\"\nbuild_number = 18\n\
             exposes = [\"ay\", \"git\"]\n\
             [[artifact]]\ntarget = \"{TRIPLE}\"\n\
             asset = \"ay-18.tar.zst\"\nsha256 = \"{sha}\"\ntree_root = \"{root}\"\n\
             size = {size}\n{row}\
             [artifact.cost]\ndisk_installed = 1048576\n"
        );
        let mut pkg = HashMap::new();
        pkg.insert(
            ("ay".to_string(), 18u64),
            (
                pkg_body.clone().into_bytes(),
                sign(&RELEASE_SEED, pkg_body.as_bytes()),
            ),
        );
        let mut archives = HashMap::new();
        archives.insert("ay-18.tar.zst".to_string(), archive);
        Fake {
            index: index_body.clone().into_bytes(),
            index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
            pkg,
            archives,
        }
    }

    /// The `(kind, protocol)` head of an https binary row.
    const HTTPS_HEAD: &str = "kind = \"binary\"\nprotocol = \"https\"\n";

    /// The vendor row every vendor-lane test starts from: a binary over https from a
    /// real allow-listed host, the tar-zst lane (the archive fixture IS a tar.zst, and
    /// that extractor predates the payload lanes), no raw-binary `entry`.
    const VENDOR_ROW: &str = "kind = \"binary\"\nprotocol = \"https\"\nurl = \"https://github.com/openai/codex/releases/download/rust-v0.150.0/codex-package-aarch64-apple-darwin.tar.zst\"\npayload = \"tar-zst\"\nvendor = \"OpenAI\"\n";

    fn layout(dir: &Path) -> Layout {
        Layout {
            prefix: dir.join("prefix"),
        }
    }

    fn tool(name: &str) -> ToolName {
        ToolName::new(name).unwrap()
    }

    /// `bin/<name>` for a name the test knows is admissible.
    fn shim_of(layout: &Layout, name: &str) -> PathBuf {
        layout.shim(&tool(name))
    }

    /// The concrete executable path a `tool` shim forwards to inside `build_dir`
    /// (`bin/<tool>` on Unix, `bin\<tool>.exe` on Windows) — what `ops::which` returns.
    fn tool_bin(build_dir: &Path, name: &str) -> PathBuf {
        build_dir.join("bin").join(tool(name).exe_file())
    }

    /// The sidecar a FAILED stage wrote goes with the build that never came to exist, and
    /// the sidecar of a build the failed stage left standing does not.
    ///
    /// Why this is a test and not an incident report: nothing else reclaims an orphan
    /// sidecar — `gc::interrupted_debris` walks `store/` for directories, and
    /// `store::discard_build` needs a tree. MEASURED 2026-09-13: after the refused pass
    /// `store/claude/` held exactly one entry, `2026091301.shim-env`, and `atpkg gc`
    /// reported nothing to reclaim. The second half is the guard that keeps the fix from
    /// being worse than the leak: a re-stage over a LIVE build fails with the old tree
    /// still in place, and that tree's shims still need its environment.
    #[test]
    fn a_failed_stage_takes_its_sidecar_and_spares_a_standing_build_s() {
        let dir = scratch("orphan-sidecar");
        let l = layout(&dir);
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        // The reference `https` row: its signed sha256 is not this archive's, so the stage
        // refuses at step 1 — before any directory is created, and before the live tree in
        // the second half could be touched.
        let art = crate::vendor::testkit::row();
        let archive = dir.join("asset");
        std::fs::write(&archive, b"not the signed bytes").unwrap();

        // Fresh install: the lane writes the sidecar (creating `store/claude/` to do it),
        // the stage fails, no build directory exists — the sidecar must not survive.
        let build_dir = l.build_dir("claude", 2_026_091_301);
        crate::shim_env::write_sidecar(&build_dir, &env).unwrap();
        assert!(
            verify_and_stage(&art, &archive, &build_dir).is_err(),
            "the archive does not match the signed sha256"
        );
        assert!(!build_dir.exists(), "no tree was staged");
        let sidecar = crate::shim_env::sidecar_path(&build_dir).unwrap();
        assert!(
            !sidecar.exists(),
            "an orphan sidecar nothing reclaims: {}",
            sidecar.display()
        );

        // Re-stage over a build that is already there: the stage fails, the standing tree
        // is untouched, and so is the environment its shims export.
        let live = l.build_dir("ay", 18);
        std::fs::create_dir_all(live.join("bin")).unwrap();
        crate::shim_env::write_sidecar(&live, &env).unwrap();
        assert!(verify_and_stage(&art, &archive, &live).is_err());
        assert!(
            live.exists(),
            "the standing tree survives a failed re-stage"
        );
        assert_eq!(
            crate::shim_env::read_sidecar(&live),
            env,
            "a standing build keeps the environment its shims export"
        );
    }

    // THE capstone: `ay` installs end-to-end from a synthetic SIGNED release — index verified
    // under the root key, pkg verified under the delegated release key, artifact downloaded,
    // sha256 + tree_root checked, activated, and shimmed (with the sensitive `git` refused).
    #[test]
    fn ay_installs_end_to_end_from_a_signed_release() {
        let dir = scratch("e2e");
        let fake = fixture(&dir);
        let layout = layout(&dir);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let report = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
        assert_eq!(report.build, 18);
        assert!(!report.already_current);
        assert_eq!(report.shimmed, vec!["ay".to_string()]);
        assert_eq!(
            report.refused_shims,
            vec!["git".to_string()],
            "sensitive shim refused"
        );
        // The binary is staged + shimmed + active.
        assert_eq!(
            crate::ops::which(&layout, "ay").unwrap(),
            tool_bin(&layout.build_dir("ay", 18), "ay")
        );
        assert!(
            crate::ops::which(&layout, "git").is_none(),
            "no sensitive shim"
        );
        assert_eq!(
            std::fs::read_link(layout.channel_current("stable")).unwrap(),
            layout.build_dir("ay", 18)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Phase integration (R5): the SAME end-to-end install, run under a live
    /// `--progress-file` pass. The hooks in `install_inner` must carry the row
    /// through the honest phase sequence and record the pinned build; the terminal
    /// snapshot must read "ended" (pid cleared), never a live-looking lie.
    ///
    /// The ONE test in this binary that touches the process-global sink (every other
    /// path runs with it disabled, which is also what keeps this isolated).
    #[test]
    fn install_phases_land_in_the_progress_file() {
        // The process-global sink has a thread owner; every test that begins a
        // pass serializes on the shared gate (see progress.rs).
        let _gate = crate::progress::PASS_TEST_GATE
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = scratch("phases");
        let fake = fixture(&dir);
        let layout = layout(&dir);
        std::fs::create_dir_all(&layout.prefix).unwrap();
        let progress = layout.prefix.join("progress.json");
        assert!(crate::progress::begin_pass(&progress, "net"));
        if let Some(sink) = crate::progress::active() {
            sink.plan(&[("ay".to_string(), 100)]);
        }
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let report = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
        assert_eq!(report.build, 18);
        if let Some(sink) = crate::progress::active() {
            sink.finished("ay", crate::progress::Phase::Done, None);
        }
        crate::progress::end_pass();
        let file: crate::progress::ProgressFile =
            aterm_json::from_str(&std::fs::read_to_string(&progress).unwrap()).unwrap();
        assert_eq!(file.pass, "net");
        assert_eq!(file.pid, None, "the pass ended — no live pid claim");
        assert!(file.ended_unix.is_some());
        let ay = &file.programs["ay"];
        assert_eq!(ay.phase, crate::progress::Phase::Done);
        assert_eq!(
            ay.build,
            Some(18),
            "the pinned build was recorded by the flow hook"
        );
        assert_eq!(file.overall.programs_done, 1);
        assert!(file.queue.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE GOVERNING STAGING INVARIANT, THROUGH THE REAL INSTALL FLOW: a re-install that
    /// fails to stage must leave the toolchain already on the machine installed, complete,
    /// and on PATH.
    ///
    /// Step 6 used to be `remove_dir_all(build_dir)` → extract, so this exact sequence — a
    /// live `ay@18`, then a re-install whose signed `tree_root` does not describe the bytes —
    /// ended with no `ay` at all, while the sibling `<build>.ready` marker the delete never
    /// touched still claimed the build was installed. The unit tests in `install.rs` pin the
    /// staging chain itself; this pins that the flow the user actually runs goes through it.
    #[test]
    fn a_restage_that_fails_leaves_the_live_toolchain_installed_and_on_path() {
        let dir = scratch("restage-keeps-toolchain");
        let layout = layout(&dir);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        install(&fixture(&dir), &layout, &anchor(), &req, fl(0), 0).unwrap();

        let build = layout.build_dir("ay", 18);
        assert!(
            crate::store::build_is_complete(&build),
            "the fixture is only interesting once ay@18 is really installed"
        );
        let ay = tool_bin(&build, "ay");
        let original = std::fs::read(&ay).unwrap();

        // The SAME archive bytes (so the sha256 gate passes and the failure lands late) under
        // a signed tree_root that cannot match — the apply-time TOCTOU re-verify, i.e. the
        // last point at which the old shape had already destroyed the live tree.
        let tampered = fixture_from(&dir, "binary", make_archive(&dir), Some(&"a".repeat(64)));
        let err = install(&tampered, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(
            matches!(err, FlowError::Stage(StageError::TreeRootMismatch { .. })),
            "the fixture must fail at the re-verify, else this proves nothing: got {err:?}"
        );

        assert!(
            crate::store::build_is_complete(&build),
            "the installed build must still be marked complete"
        );
        assert_eq!(
            std::fs::read(&ay).unwrap(),
            original,
            "the live binary must be the ORIGINAL one, byte for byte"
        );
        assert_eq!(
            crate::ops::which(&layout, "ay").unwrap(),
            ay,
            "and it must still be what the shim forwards to"
        );
        assert_eq!(
            crate::ops::list_installed(&layout),
            vec![("ay".to_string(), 18u64)],
            "the store must still report exactly the build that is really there"
        );
        assert_eq!(
            std::fs::read_link(layout.channel_current("stable")).unwrap(),
            build
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every entry in `prefix/staging/<program>/`, sorted.
    fn staged_assets(layout: &Layout, program: &str) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(layout.staging_dir(program)) else {
            return Vec::new();
        };
        let mut out: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .collect();
        out.sort();
        out
    }

    // THE COMPRESSED ASSET OF A FAILED STAGE IS RETAINED — ONE COPY, REUSED — WHEN THE
    // BYTES WERE RIGHT, and reclaimed with its `.part` when they were not (2026-09-15).
    // Before: reclaimed on every exit, so a stage that failed for a reason that was not
    // the bytes' (the provenance refusal of 2026-09-14, a full disk) re-downloaded the
    // same archive on every launch and every six-hour tick — 743 MB per pass for `trust`
    // on the owner's machine. The strandage bound the old rule guarded still holds:
    // `sweep_foreign_partials` keeps one archive per program, this pass's.
    #[test]
    fn a_failed_stage_retains_one_verified_archive_and_a_digest_failure_none() {
        let dir = scratch("staging-retain");
        let layout = layout(&dir);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        // The same archive bytes under a signed tree_root that cannot match: the sha256 gate
        // passes, so the download really lands in `staging/` and the failure is late.
        let tampered = fixture_from(&dir, "binary", make_archive(&dir), Some(&"a".repeat(64)));
        let retained = layout
            .prefix
            .join("staging")
            .join("ay")
            .join("ay-18.tar.zst");
        let mut identity: Option<(u64, u64)> = None;
        for _ in 0..3 {
            let err = install(&tampered, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
            assert!(
                matches!(err, FlowError::Stage(StageError::TreeRootMismatch { .. })),
                "PRECONDITION: the stage must fail AFTER the download: {err:?}"
            );
            assert_eq!(
                staged_assets(&layout, "ay"),
                vec!["ay-18.tar.zst".to_string()],
                "the verified archive is retained, once"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                let meta = std::fs::metadata(&retained).unwrap();
                let now = (meta.dev(), meta.ino());
                if let Some(prev) = identity {
                    assert_eq!(
                        prev, now,
                        "the retained archive is reused, never re-fetched"
                    );
                }
                identity = Some(now);
            }
        }
        // A DIGEST failure keeps nothing: the bytes are wrong, and a `.part` of them would
        // seed the next attempt. A fresh store, because a retained archive whose digest is
        // the row's is reused before any download.
        let ddir = scratch("staging-digest");
        let dlayout = Layout {
            prefix: ddir.join("prefix"),
        };
        std::fs::create_dir_all(&dlayout.prefix).unwrap();
        let archive = make_archive(&ddir);
        let good = fixture_from(&ddir, "binary", archive.clone(), None);
        std::fs::write(&archive, b"not the bytes the row signed").unwrap();
        let err = install(&good, &dlayout, &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(
            matches!(err, FlowError::Stage(StageError::Sha256Mismatch { .. })),
            "PRECONDITION: the digest gate must fail: {err:?}"
        );
        assert!(
            staged_assets(&dlayout, "ay").is_empty(),
            "a digest failure reclaims the archive: {:?}",
            staged_assets(&dlayout, "ay")
        );
        let _ = std::fs::remove_dir_all(&ddir);

        // Non-vacuity, and the reuse itself: the good row has the SAME bytes under the
        // right root, so the retained archive is what it stages — no fetch — and a
        // successful install leaves staging empty as it always did.
        install(&fixture(&dir), &layout, &anchor(), &req, fl(0), 0).unwrap();
        assert!(staged_assets(&layout, "ay").is_empty());
        assert!(crate::store::build_is_complete(&layout.build_dir("ay", 18)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // THE SAME BLIND SPOT ON THE SINGLE-PROGRAM PATH. `abort_activated_install` unwinds an
    // install that failed AFTER activation, and its discard is sound for a build this call
    // created. It is not sound for one that was already live: the failures that land there
    // are `install_tools` (an EACCES/EROFS/full disk while writing `bin/` shims — a fact
    // about the environment, not the tree) and the resolve check, and deleting a verified
    // multi-gigabyte tree in response leaves the user with no toolchain at all.
    #[test]
    fn a_post_activation_abort_never_discards_a_build_that_was_already_live() {
        let dir = scratch("single-abort-live");
        let ndir = scratch("single-abort-fresh");
        // Both layouts before the binding shadows the constructor.
        let nlayout = layout(&ndir);
        let layout = layout(&dir);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        install(&fixture(&dir), &layout, &anchor(), &req, fl(0), 0).unwrap();
        let build = layout.build_dir("ay", 18);
        assert!(
            crate::store::build_is_complete(&build),
            "PRECONDITION: ay@18 is really installed"
        );
        assert_eq!(
            std::fs::read_link(layout.program_current("ay")).unwrap(),
            build,
            "PRECONDITION: and really live"
        );

        // Break `install_tools` the way an unwritable prefix does: a regular FILE where the
        // `bin/` directory must be, so `ensure_private_dir` fails AFTER activation. The
        // `installed: None` request is the shim view being silent about a live program.
        std::fs::remove_dir_all(layout.bin_dir()).unwrap();
        std::fs::write(layout.bin_dir(), b"not a dir").unwrap();

        let err = install(&fixture(&dir), &layout, &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(
            matches!(err, FlowError::Activate(_)),
            "PRECONDITION: the failure must land after activation: {err:?}"
        );

        assert!(
            build.is_dir(),
            "a failed shim write destroyed the live toolchain tree"
        );
        assert_eq!(
            std::fs::read(tool_bin(&build, "ay")).unwrap(),
            b"#!/bin/true\nay",
            "and the tree left behind is the verified one"
        );
        assert!(
            crate::store::build_is_complete(&build),
            "it must still read as installed, so the retry re-stages it in place"
        );

        // AND THE KEEP MUST OUTLIVE THE RUN. `was_live` is read off the two `current`
        // links, and the unwind removes both — so the retry below used to probe
        // `was_live == false` and the SECOND failure of the same unwritable `bin/` fell
        // through to the discard, deleting the tree the first abort kept. The witness is
        // re-pointed at the kept build precisely so the next attempt can still tell.
        assert_eq!(
            std::fs::read_link(layout.program_current("ay")).unwrap(),
            build,
            "the kept tree lost its witness, so the retry cannot know it was ever live"
        );
        let again = install(&fixture(&dir), &layout, &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(
            matches!(again, FlowError::Activate(_)),
            "PRECONDITION: the retry fails the same way: {again:?}"
        );
        assert!(
            crate::store::build_is_complete(&build),
            "a SECOND abort with `bin/` still unwritable discarded the live toolchain tree"
        );
        assert_eq!(
            std::fs::read(tool_bin(&build, "ay")).unwrap(),
            b"#!/bin/true\nay",
            "and the tree that survived the retry is still the verified one"
        );
        assert_eq!(
            std::fs::read_link(layout.program_current("ay")).unwrap(),
            build,
            "and the keep stays durable for the attempt after that"
        );

        // Non-vacuity: a build this call really did create IS still discarded. Restore the
        // prefix, then fail a FRESH install of a program that was never live.
        std::fs::remove_file(layout.bin_dir()).unwrap();
        let ndir = scratch("single-abort-fresh");
        std::fs::create_dir_all(nlayout.prefix.join("store")).unwrap();
        std::fs::write(nlayout.bin_dir(), b"not a dir").unwrap();
        let nerr = install(&fixture(&ndir), &nlayout, &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(matches!(nerr, FlowError::Activate(_)), "got {nerr:?}");
        assert!(
            !nlayout.build_dir("ay", 18).exists(),
            "a build the failed install itself created is still discarded"
        );
        let _ = std::fs::remove_dir_all(&ndir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The group path leaks the same way and must reclaim the same way for the member that
    // FAILED: ITS archive is the one nobody ever comes back for. Its SIBLINGS are the
    // opposite case — the next pass comes back for exactly those; see the carry test below.
    #[test]
    fn an_aborted_group_reclaims_the_failing_members_archive() {
        let dir = scratch("staging-leak-group");
        let fake = group_fixture(&dir);
        let layout = layout(&dir);
        // trust's archive no longer matches its signed sha256 → its stage fails, after the
        // download has already put the bytes in `staging/trust/`.
        std::fs::write(fake.archives.get("trust-4821.tar.zst").unwrap(), b"corrupt").unwrap();

        let report = apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &std::collections::BTreeMap::from([("ay".to_string(), 17u64)]),
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert!(
            matches!(report.groups[0].1, TxnOutcome::Aborted { .. }),
            "PRECONDITION: the group aborted: {:?}",
            report.groups[0].1
        );
        assert!(
            staged_assets(&layout, "trust").is_empty(),
            "the failing member stranded its archive: {:?}",
            staged_assets(&layout, "trust")
        );
        assert_eq!(
            staged_assets(&layout, "ay"),
            vec!["ay-18.tar.zst".to_string()],
            "…but the member that staged fine BEFORE the abort KEEPS its verified archive: \
             the abort takes its extracted tree (it must), and taking the download with it \
             made the next pass refetch a member that never failed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // THE RETRY IS BOUNDED TO WHAT ACTUALLY FAILED. A coherence group aborts all-or-nothing
    // and discards every staged sibling's extracted tree — that invariant is not negotiable
    // (a complete-but-inactive build would be mis-read as the active build and split the
    // tuple). Discarding their DOWNLOADS along with it is what made a flaky link
    // unaffordable: a 4-member tuple failing on member 3 refetched members 1-2 from byte 0
    // on every tick, the biggest of them a ~3.4 GB toolchain, so a metered or slow link
    // could leave the tuple never converging — while the resume-across-passes guarantee
    // (R4) covered only the ONE member whose own download failed.
    //
    // The saving is made OBSERVABLE the only way a local-copy fetcher can show it: pass 2
    // runs with the sibling's asset REMOVED from the registry, so the sole way it can stage
    // is from the archive pass 1 carried. Before this fix pass 2 aborted.
    #[test]
    fn an_aborted_tuple_carries_its_verified_siblings_so_the_retry_refetches_only_the_failure() {
        let dir = scratch("group-carry-archive");
        let mut fake = group_fixture(&dir);
        let layout = layout(&dir);
        let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);

        // PASS 1: trust's asset is unfetchable (the flaky-link case). `ay` stages first and
        // COMPLETELY — downloaded, sha256-verified, tree_root-verified, extracted — and
        // then trust's download fails and takes the whole tuple down with it.
        let good_trust = fake.archives.remove("trust-4821.tar.zst").unwrap();
        let report = apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &installed,
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert!(
            matches!(report.groups[0].1, TxnOutcome::Aborted { .. }),
            "PRECONDITION: the group aborted: {:?}",
            report.groups[0].1
        );
        assert!(
            !layout.build_dir("ay", 18).exists(),
            "PRECONDITION: the abort still discards the sibling's extracted TREE"
        );
        assert_eq!(
            staged_assets(&layout, "ay"),
            vec!["ay-18.tar.zst".to_string()],
            "the sibling's verified archive is carried to the next pass"
        );

        // The pass-end gc must not undo that — the CLI's wiring, verbatim.
        let _ = crate::gc::run_keeping_pinned_partials(&layout, &|p| {
            report.resolved_assets.get(p).cloned()
        });
        assert_eq!(
            staged_assets(&layout, "ay"),
            vec!["ay-18.tar.zst".to_string()],
            "the pass-closing sweep spares the carried archive, as it does the `.part`"
        );

        // PASS 2: trust is reachable again, but `ay`'s asset is GONE from the registry, so
        // re-downloading it cannot succeed. The tuple must converge anyway.
        fake.archives
            .insert("trust-4821.tar.zst".to_string(), good_trust);
        fake.archives.remove("ay-18.tar.zst");
        let retry = apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &installed,
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert!(
            matches!(retry.groups[0].1, TxnOutcome::Applied(_)),
            "the retry must re-stage the sibling from its carried archive instead of \
             re-downloading it: {:?}",
            retry.groups[0].1
        );
        assert!(crate::store::build_is_complete(&layout.build_dir("ay", 18)));
        assert!(crate::store::build_is_complete(
            &layout.build_dir("trust", 4821)
        ));

        // A SETTLED tuple keeps nothing: the carry exists only BETWEEN passes, so the
        // strandage bound the failing-member test pins is not widened by any of this.
        assert!(
            staged_assets(&layout, "ay").is_empty(),
            "a settled tuple reclaims the carried archive: {:?}",
            staged_assets(&layout, "ay")
        );
        assert!(
            staged_assets(&layout, "trust").is_empty(),
            "{:?}",
            staged_assets(&layout, "trust")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // THE RESUME-ACROSS-PASSES WIRING (R4), update lane: a member whose DOWNLOAD fails
    // mid-pass reaches `ChannelApplyReport::resolved_assets` anyway — the name is recorded
    // when the verified manifest selects the artifact, BEFORE any byte moves — so the CLI's
    // pass-end `gc::run_keeping_pinned_partials`, fed exactly that map, spares the `.part`
    // the fetcher left and the next pass resumes instead of refetching from byte 0.
    #[test]
    fn a_failed_download_survives_its_own_pass_end_gc_and_a_plain_gc_still_sweeps_it() {
        let dir = scratch("resume-survives-pass-end");
        let mut fake = group_fixture(&dir);
        let layout = layout(&dir);
        // trust's asset is unfetchable: the download fails AFTER its manifest verified —
        // the window that leaves a `.part` behind in production. `Fake` copies whole
        // files or fails, so the test seeds the resume bytes the real fetcher would leave.
        fake.archives.remove("trust-4821.tar.zst");
        let staging = layout.staging_dir("trust");
        std::fs::create_dir_all(&staging).unwrap();
        let part = staging.join("trust-4821.tar.zst.part");
        std::fs::write(&part, b"resume-bytes").unwrap();

        let report = apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &std::collections::BTreeMap::from([("ay".to_string(), 17u64)]),
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert!(
            matches!(report.groups[0].1, TxnOutcome::Aborted { .. }),
            "PRECONDITION: the failed download aborted the group: {:?}",
            report.groups[0].1
        );
        assert!(
            part.exists(),
            "PRECONDITION: the resume state survived the abort"
        );
        // The FAILED member is in the map — resolution happened before its download —
        // and so is the sibling that staged fine (harmless: it has no `.part` left).
        assert_eq!(
            report.resolved_assets.get("trust").map(String::as_str),
            Some("trust-4821.tar.zst"),
            "the failed member's pinned asset name must reach the report"
        );
        assert_eq!(
            report.resolved_assets.get("ay").map(String::as_str),
            Some("ay-18.tar.zst")
        );

        // The CLI's pass-end wiring, verbatim: the sparing closure IS the report's map.
        let _ = crate::gc::run_keeping_pinned_partials(&layout, &|p| {
            report.resolved_assets.get(p).cloned()
        });
        assert!(
            part.exists(),
            "the pass-end sweep spares the failed member's resume state"
        );

        // `uninstall --all` and the standalone `atpkg gc` keep the PLAIN form on purpose
        // (the user asked for the disk back): everything spared above stays reclaimable.
        let _ = crate::gc::run(&layout);
        assert!(
            !part.exists(),
            "a plain gc still reclaims the spared partial"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The same wiring on the singleton lane (`bootstrap_singleton` / `do_install`): a
    // failed install returns NO report, so the resolved asset rides the collector
    // out-param — filled before the download, surviving the `Err`.
    #[test]
    fn a_failed_singleton_download_still_fills_the_resolved_asset_collector() {
        let dir = scratch("resume-collector-singleton");
        let mut fake = fixture(&dir);
        fake.archives.remove("ay-18.tar.zst");
        let layout = layout(&dir);
        let staging = layout.staging_dir("ay");
        std::fs::create_dir_all(&staging).unwrap();
        let part = staging.join("ay-18.tar.zst.part");
        std::fs::write(&part, b"resume-bytes").unwrap();
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };

        let mut resolved = std::collections::BTreeMap::new();
        let err =
            install_collecting_assets(&fake, &layout, &anchor(), &req, fl(0), 0, &mut resolved)
                .unwrap_err();
        assert!(
            matches!(err, FlowError::Download(_)),
            "PRECONDITION: the download failed: {err:?}"
        );
        assert_eq!(
            resolved.get("ay").map(String::as_str),
            Some("ay-18.tar.zst"),
            "the collector must survive the Err path — that is its whole reason to exist"
        );

        let _ = crate::gc::run_keeping_pinned_partials(&layout, &|p| resolved.get(p).cloned());
        assert!(part.exists(), "the pass-end sweep spares the resume state");
        let _ = crate::gc::run(&layout);
        assert!(!part.exists(), "a plain gc still reclaims it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The FETCHER CONTRACT for the vendor lane: `download_url` is refused by default —
    /// a test fetcher and the `dir:` registry fetcher never reach the network by accident.
    #[test]
    fn a_fetcher_refuses_vendor_urls_unless_it_opts_in() {
        struct Inert;
        impl Fetcher for Inert {
            fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
                Err("inert".into())
            }
            fn pkg_manifest(&self, _: &str, _: &str, _: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
                Err("inert".into())
            }
            fn download(&self, _: &str, _: &str, _: &Path) -> Result<(), String> {
                Err("inert".into())
            }
        }
        let dest = std::env::temp_dir().join("atpkg-flow-inert-never-written");
        let err = Inert
            .download_url("https://github.com/x/y", &dest, 1)
            .unwrap_err();
        assert!(err.contains("cannot fetch vendor URLs"), "{err}");
        let dir_fetcher = crate::net::DirFetcher::new(std::env::temp_dir());
        assert!(
            dir_fetcher
                .download_url("https://github.com/x/y", &dest, 1)
                .is_err(),
            "the dir: registry keeps the fail-closed default"
        );
        assert!(!dest.exists(), "a refusal writes nothing");
    }

    /// A `github-release` row's `asset` is joined onto `staging/<program>/`, and the flow
    /// unlinks that path and sweeps every `*.part` beside it BEFORE any download. An asset
    /// that is a PATH — absolute (`Path::join` replaces the base) or climbing with `..` —
    /// must be refused before either runs, so a signed row can never delete a file outside
    /// staging (audit K3, 2026-09-12).
    #[test]
    fn install_never_unlinks_outside_staging_for_a_release_row() {
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        for label in ["absolute", "climbing"] {
            let dir = scratch(&format!("release-asset-{label}"));
            let victim = dir.join("victim");
            std::fs::create_dir_all(&victim).unwrap();
            std::fs::write(victim.join("keep.txt"), b"keep").unwrap();
            std::fs::write(victim.join("other.part"), b"part").unwrap();
            // `layout(dir)` stages under `dir/prefix/staging/ay/`.
            let asset = if label == "absolute" {
                victim.join("keep.txt").to_string_lossy().into_owned()
            } else {
                String::from("../../../victim/keep.txt")
            };
            let base = fixture(&dir);
            let sha = "a".repeat(64);
            let root = "b".repeat(64);
            let pkg_body = format!(
                "schema = 2\nprogram = \"ay\"\nversion = \"0.1\"\nbuild_number = 18\n\
                 exposes = [\"ay\"]\n\
                 [[artifact]]\ntarget = \"{TRIPLE}\"\nkind = \"binary\"\n\
                 protocol = \"github-release\"\nasset = \"{asset}\"\n\
                 sha256 = \"{sha}\"\ntree_root = \"{root}\"\nsize = 1\n\
                 [artifact.cost]\ndisk_installed = 1\n"
            );
            let mut pkg = HashMap::new();
            pkg.insert(
                ("ay".to_string(), 18u64),
                (
                    pkg_body.clone().into_bytes(),
                    sign(&RELEASE_SEED, pkg_body.as_bytes()),
                ),
            );
            let fake = Fake {
                index: base.index,
                index_sig: base.index_sig,
                pkg,
                archives: HashMap::new(),
            };
            let err = install(&fake, &layout(&dir), &anchor(), &req, fl(0), 0).unwrap_err();
            assert!(
                victim.join("keep.txt").exists(),
                "{label}: the file named by the asset was unlinked ({err:?})"
            );
            assert!(
                victim.join("other.part").exists(),
                "{label}: a partial beside it was swept ({err:?})"
            );
            assert!(
                matches!(err, FlowError::VendorRefused(_)),
                "{label}: refused as a row, not a download: {err:?}"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// THE VENDOR LANE, end to end through the real flow:
    /// 1. a `binary` row over the `https` protocol is DISPATCHED (never
    ///    `UnsupportedKind`) and admitted BEFORE any byte moves — a row with no url is
    ///    `VendorRefused` and the staging dir stays empty;
    /// 2. an admitted row downloads through `download_url` ONLY: the slug lane
    ///    (`download`/`download_for`) is never consulted, and a fetcher without the vendor
    ///    lane fails as a `Download` naming it, leaving nothing installed;
    /// 3. a fetcher that serves the signed URL installs, shims and activates exactly like a
    ///    plain binary, under the signed sha256 + tree_root gates, with the signed `size`
    ///    as the exact cap and NO account slug or asset name on the request.
    #[test]
    fn an_https_row_is_admitted_then_downloaded_only_through_the_vendor_lane() {
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        // 1. Refused closed at admission.
        let rdir = scratch("vendor-refused");
        let refused = fixture_vendor(&rdir, HTTPS_HEAD);
        let rlay = layout(&rdir);
        let err = install(&refused, &rlay, &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(
            matches!(err, FlowError::VendorRefused(_)),
            "dispatched and refused at admission, not UnsupportedKind: {err:?}"
        );
        assert!(err.to_string().contains("url must be https"), "{err}");
        assert!(
            !rlay.staging_dir("ay").exists(),
            "admission runs before the staging dir is even created"
        );
        // A host outside the allow-list is refused the same way, even over https.
        let hdir = scratch("vendor-host");
        let hostile = fixture_vendor(
            &hdir,
            "kind = \"binary\"\nprotocol = \"https\"\n\
             url = \"https://evil.example/codex.tar.zst\"\npayload = \"tar-zst\"\n",
        );
        let err = install(&hostile, &layout(&hdir), &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(matches!(err, FlowError::VendorRefused(_)), "{err:?}");
        assert!(
            err.to_string().contains("not an allow-listed vendor host"),
            "{err}"
        );

        // 2. Admitted, but this fetcher has no vendor lane: the slug lane is NOT a fallback.
        let ndir = scratch("vendor-no-lane");
        let no_lane = fixture_vendor(&ndir, VENDOR_ROW);
        let nlay = layout(&ndir);
        let err = install(&no_lane, &nlay, &anchor(), &req, fl(0), 0).unwrap_err();
        match &err {
            FlowError::Download(m) => assert!(m.contains("cannot fetch vendor URLs"), "{m}"),
            other => panic!("expected the vendor lane's refusal, got {other:?}"),
        }
        assert!(
            crate::ops::which(&nlay, "ay").is_none(),
            "nothing installed"
        );

        // 3. A fetcher WITH the vendor lane installs end to end — and only that lane is used.
        struct VendorFake {
            inner: Fake,
            served: PathBuf,
            seen: RefCell<Vec<(String, u64)>>,
        }
        impl Fetcher for VendorFake {
            fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
                self.inner.index_candidates()
            }
            fn pkg_manifest(&self, r: &str, p: &str, b: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
                self.inner.pkg_manifest(r, p, b)
            }
            fn download(&self, _: &str, asset: &str, _: &Path) -> Result<(), String> {
                panic!("a vendor row must never reach the slug lane (asked for {asset})");
            }
            fn download_url(&self, url: &str, dest: &Path, cap: u64) -> Result<(), String> {
                self.seen.borrow_mut().push((url.to_string(), cap));
                std::fs::copy(&self.served, dest)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
        }
        let vdir = scratch("vendor-lane");
        let inner = fixture_vendor(&vdir, VENDOR_ROW);
        let served = inner.archives["ay-18.tar.zst"].clone();
        let size = std::fs::metadata(&served).unwrap().len();
        let vendor = VendorFake {
            inner,
            served,
            seen: RefCell::new(Vec::new()),
        };
        let vlay = layout(&vdir);
        let report = install(&vendor, &vlay, &anchor(), &req, fl(0), 0).unwrap();
        assert_eq!(report.build, 18);
        assert_eq!(report.shimmed, vec!["ay".to_string()]);
        assert_eq!(
            crate::ops::which(&vlay, "ay").unwrap(),
            tool_bin(&vlay.build_dir("ay", 18), "ay"),
            "activated and shimmed like a plain binary"
        );
        let seen = vendor.seen.borrow();
        assert_eq!(seen.len(), 1, "exactly one vendor download");
        assert!(
            seen[0]
                .0
                .starts_with("https://github.com/openai/codex/releases/download/"),
            "the SIGNED url, verbatim: {}",
            seen[0].0
        );
        assert_eq!(seen[0].1, size, "the cap is the signed size, exactly");
        for d in [rdir, hdir, ndir, vdir] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// THE RELEASE LANE IS CAPPED BY THE ROW'S SIGNED SIZE, not by the fetcher's own
    /// ceiling — the property the vendor lane above has always had, now proven for
    /// `github-release` too.
    ///
    /// `disk_gate(size + disk_installed)` is computed from the signed number and is
    /// checked ONCE, before the transfer; nothing bounds the bytes as they land but the
    /// cap the fetcher is given, and the sha256 gate sees the file only after it is
    /// whole. A release lane capped at its own 8 GiB constant therefore let a
    /// mis-uploaded or substituted asset (GitHub takes up to 2 GiB per asset) write
    /// through the free-space floor the preflight had just defended — for a row whose
    /// signed size says 100 bytes — and refused it only afterwards. So the number the
    /// flow hands `download_for` is the row's own `size`, exactly as `download_url` has
    /// always taken it.
    #[test]
    fn a_github_release_download_is_capped_at_the_rows_signed_size() {
        struct CapFake {
            inner: Fake,
            seen: RefCell<Vec<(String, u64)>>,
        }
        impl Fetcher for CapFake {
            fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
                self.inner.index_candidates()
            }
            fn pkg_manifest(&self, r: &str, p: &str, b: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
                self.inner.pkg_manifest(r, p, b)
            }
            fn download(&self, _: &str, asset: &str, _: &Path) -> Result<(), String> {
                panic!(
                    "the artifact lane is `download_for`, which carries the cap (asked for {asset})"
                );
            }
            fn download_for(
                &self,
                _program: &str,
                repo: &str,
                asset: &str,
                dest: &Path,
                cap: u64,
            ) -> Result<(), String> {
                self.seen.borrow_mut().push((asset.to_string(), cap));
                self.inner.download(repo, asset, dest)
            }
        }
        let dir = scratch("release-lane-cap");
        // The fixture's signed row states `size = 100` — a number nothing else in the
        // flow reads back, so an assertion on it can only be satisfied by the SIGNED
        // value actually reaching the lane.
        let f = CapFake {
            inner: fixture(&dir),
            seen: RefCell::new(Vec::new()),
        };
        let lay = layout(&dir);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let report = install(&f, &lay, &anchor(), &req, fl(0), 0).unwrap();
        assert_eq!(report.build, 18);
        let seen = f.seen.borrow();
        assert_eq!(seen.len(), 1, "exactly one artifact download");
        assert_eq!(seen[0].0, "ay-18.tar.zst");
        assert_eq!(
            seen[0].1, 100,
            "the cap is the row's SIGNED size, never the fetcher's 8 GiB ceiling"
        );
        drop(seen);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// DESIGN S7, end to end through the real flow: a manifest declaring
    /// `shim_env = ["DISABLE_AUTOUPDATER=1"]` installs like any vendor row, and the shim
    /// it lays EXPORTS that variable before it execs — read back off the shim, recorded
    /// beside the build in its `<build>.shim-env` sidecar (so the verbs that hold no
    /// manifest re-lay it the same way), and still a shim `which` resolves. A manifest
    /// whose `shim_env` breaks the rule is refused at PARSE as `ShimEnv` naming the entry,
    /// before any byte moves.
    #[test]
    fn a_shim_env_manifest_lays_an_exporting_shim_and_a_bad_one_is_refused() {
        struct VendorFake {
            inner: Fake,
            served: PathBuf,
        }
        impl Fetcher for VendorFake {
            fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
                self.inner.index_candidates()
            }
            fn pkg_manifest(&self, r: &str, p: &str, b: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
                self.inner.pkg_manifest(r, p, b)
            }
            fn download(&self, _: &str, asset: &str, _: &Path) -> Result<(), String> {
                panic!("a vendor row must never reach the slug lane (asked for {asset})");
            }
            fn download_url(&self, _: &str, dest: &Path, _: u64) -> Result<(), String> {
                std::fs::copy(&self.served, dest)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
        }
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        // `fixture_vendor` appends `row` INSIDE the artifact table; a top-level key has to
        // come before it, so re-head the signed body: the same bytes with `shim_env` after
        // `exposes`, re-signed.
        let with_env = |dir: &Path, list: &str| {
            let mut fake = fixture_vendor(dir, VENDOR_ROW);
            let (body, _) = fake.pkg.remove(&("ay".to_string(), 18u64)).unwrap();
            let body = String::from_utf8(body).unwrap().replacen(
                "exposes = [\"ay\", \"git\"]\n",
                &format!("exposes = [\"ay\", \"git\"]\nshim_env = {list}\n"),
                1,
            );
            fake.pkg.insert(
                ("ay".to_string(), 18u64),
                (
                    body.clone().into_bytes(),
                    sign(&RELEASE_SEED, body.as_bytes()),
                ),
            );
            fake
        };
        let dir = scratch("shim-env-lane");
        let inner = with_env(&dir, "[\"DISABLE_AUTOUPDATER=1\"]");
        let served = inner.archives["ay-18.tar.zst"].clone();
        let vendor = VendorFake { inner, served };
        let l = layout(&dir);
        let report = install(&vendor, &l, &anchor(), &req, fl(0), 0).unwrap();
        assert_eq!(report.build, 18);
        let shim = l.shim(&tool("ay"));
        assert_eq!(
            crate::ops::which(&l, "ay").unwrap(),
            tool_bin(&l.build_dir("ay", 18), "ay"),
            "still a shim that resolves"
        );
        let env = crate::platform::shim_env_of(&shim);
        assert_eq!(
            env.spelled(),
            "DISABLE_AUTOUPDATER=1",
            "the shim exports it"
        );
        assert_eq!(
            env.fix_line().as_deref(),
            Some("self-update off (DISABLE_AUTOUPDATER=1)")
        );
        assert_eq!(
            crate::shim_env::read_sidecar(&l.build_dir("ay", 18)),
            env,
            "recorded beside the build for the verbs that hold no manifest"
        );
        #[cfg(unix)]
        {
            let body = std::fs::read_to_string(&shim).unwrap();
            assert!(
                body.contains("\nexport DISABLE_AUTOUPDATER='1'\nexec '"),
                "the export precedes the exec: {body}"
            );
        }
        // The rollback verb's path re-lays the PRIOR build's shims from ITS sidecar: a
        // newer build without the key flips live, then rolls back — the env returns.
        let b19 = bare_build(&l, "ay", 19, &["ay"]);
        crate::shim_env::write_sidecar(&b19, &crate::shim_env::ShimEnv::NONE).unwrap();
        activate_channel(&l, "stable", &b19).unwrap();
        install_tools(&l, &b19, &[tool("ay")], Aliases::Off).unwrap();
        assert_eq!(
            crate::platform::shim_env_of(&shim),
            crate::shim_env::ShimEnv::NONE,
            "19 declares no env"
        );
        let staged = Staged {
            build: 19,
            build_dir: b19,
            exposes: vec![tool("ay")],
            prior_build: Some(18),
            was_live: false,
            reloc: None,
            tree_root: String::new(),
            aliases: Aliases::Off,
        };
        rollback_member(&l, "stable", "ay", &staged);
        assert!(
            crate::platform::resolve_shim(&shim)
                .is_some_and(|t| t.starts_with(l.build_dir("ay", 18)))
        );
        assert_eq!(
            crate::platform::shim_env_of(&shim).spelled(),
            "DISABLE_AUTOUPDATER=1",
            "the prior build's env, from its sidecar"
        );
        // The sidecar goes with the tree.
        crate::store::discard_build(&l.build_dir("ay", 18));
        assert_eq!(
            crate::shim_env::read_sidecar(&l.build_dir("ay", 18)),
            crate::shim_env::ShimEnv::NONE
        );
        let _ = std::fs::remove_dir_all(&dir);

        // A `shim_env` the rule refuses: refused at parse, by name, nothing staged.
        let bdir = scratch("shim-env-refused");
        let bad = with_env(&bdir, "[\"PATH=/evil\"]");
        let served = bad.archives["ay-18.tar.zst"].clone();
        let bl = layout(&bdir);
        let err = install(
            &VendorFake { inner: bad, served },
            &bl,
            &anchor(),
            &req,
            fl(0),
            0,
        )
        .unwrap_err();
        match &err {
            FlowError::ShimEnv(why) => assert!(why.contains("PATH=/evil"), "{why}"),
            other => panic!("expected ShimEnv, got {other:?}"),
        }
        assert!(
            err.to_string().contains("manifest refused: shim_env entry"),
            "{err}"
        );
        assert!(
            !bl.staging_dir("ay").exists(),
            "refused before any byte moved"
        );
        assert!(crate::ops::which(&bl, "ay").is_none());
        let _ = std::fs::remove_dir_all(&bdir);
    }

    /// The retired `kind = "vendor-fetch"` spelling is refused at PARSE — the flow sees
    /// `RetiredKind`, whose words name the split (never a dispatch, never a download) —
    /// and nothing is staged.
    #[test]
    fn the_retired_vendor_fetch_kind_is_a_parse_refusal() {
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let dir = scratch("vendor-fetch-retired");
        let fake = fixture_vendor(
            &dir,
            "kind = \"vendor-fetch\"\nurl = \"https://github.com/x/y.tar.zst\"\npayload = \"tar-zst\"\n",
        );
        let lay = layout(&dir);
        let err = install(&fake, &lay, &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(matches!(err, FlowError::RetiredKind(_)), "{err:?}");
        let words = err.to_string();
        assert!(
            words.starts_with("manifest refused: kind = \"vendor-fetch\" is retired"),
            "{words}"
        );
        assert!(words.contains("kind = \"binary\""), "{words}");
        assert!(words.contains("protocol = \"https\""), "{words}");
        assert!(!lay.staging_dir("ay").exists());
        assert!(crate::ops::which(&lay, "ay").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `pkg` and `system-pm` rows are ADMITTED (the schema and `check_row` accept them)
    /// and DISPATCHED to their own strategies: a `system-pm` row reaches its lane, which
    /// — with the manager absent from the (injected, empty) `PATH` — answers the
    /// canonical `unavailable on <target>` outcome, moves no byte, stages nothing and
    /// runs nothing; a `pkg` row reaches its lane, which DEFERS under this test's
    /// (default) elevation. A row that fails its own field checks is still
    /// `VendorRefused` first.
    #[test]
    fn pkg_and_system_pm_rows_are_admitted_and_dispatched_to_their_lanes() {
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let pdir = scratch("proto-pkg");
        let pkg = fixture_vendor(
            &pdir,
            "kind = \"installer-pkg\"\nprotocol = \"pkg\"\n\
             url = \"https://github.com/Homebrew/brew/releases/download/4.5.0/Homebrew-4.5.0.pkg\"\n\
             signer_team = \"927JGANW46\"\nelevated = true\nprovides = [\"/opt/homebrew/bin/brew\"]\n\
             vendor = \"Homebrew\"\n",
        );
        // `fixture_vendor` writes a tree_root, which a pkg row may not carry: strip it by
        // hand-checking the refusal names that field, then use the real shape.
        let play = layout(&pdir);
        let err = install(&pkg, &play, &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(
            matches!(&err, FlowError::VendorRefused(m) if m.contains("nothing lands in the store")),
            "{err:?}"
        );
        assert!(!play.staging_dir("ay").exists());
        let _ = std::fs::remove_dir_all(&pdir);

        let sdir = scratch("proto-system-pm");
        // A system-pm row carries no digests at all, so build it without the fixture's
        // sha256/tree_root/size: the index and pkg bodies by hand.
        let index_body = format!(
            "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
             [programs.ay]\nrepo = \"ay\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             pin = {{ ay = 18 }}\n",
            attr = attribution()
        );
        let pkg_body = format!(
            "schema = 2\nprogram = \"ay\"\nversion = \"0.1\"\nbuild_number = 18\n\
             exposes = [\"ay\"]\n\
             [[artifact]]\ntarget = \"{TRIPLE}\"\nkind = \"system-package\"\n\
             protocol = \"system-pm\"\nmanager = \"brew\"\npackage = \"ay\"\n\
             provides = [\"ay\"]\n"
        );
        let mut pkgs = HashMap::new();
        pkgs.insert(
            ("ay".to_string(), 18u64),
            (
                pkg_body.clone().into_bytes(),
                sign(&RELEASE_SEED, pkg_body.as_bytes()),
            ),
        );
        let fake = Fake {
            index: index_body.clone().into_bytes(),
            index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
            pkg: pkgs,
            archives: HashMap::new(),
        };
        let slay = layout(&sdir);
        let rec = Rc::new(crate::elevate::testkit::Recorder::new(vec![]));
        let empty = std::ffi::OsString::new();
        let report = crate::elevate::with_path_var(Some(&empty), || {
            crate::elevate::with_runner(rec.clone(), || {
                install(&fake, &slay, &anchor(), &req, fl(0), 0).unwrap()
            })
        });
        assert_eq!(
            report.protocol,
            Some(ProtocolOutcome::Unavailable {
                protocol: "brew",
                target: TRIPLE.to_string(),
                hint: crate::system_pm::missing_manager_hint(
                    crate::vendor::manager("brew").unwrap(),
                    "ay",
                    None
                ),
            })
        );
        assert_eq!(
            report.protocol.as_ref().unwrap().state("ay"),
            "unavailable on aarch64-apple-darwin: brew is not on PATH (the pinned row \
             installs ay through it, and atpkg never installs a package manager)"
        );
        assert!(rec.argvs().is_empty(), "no manager ran");
        assert!(!slay.staging_dir("ay").exists(), "no byte moved");
        assert!(crate::ops::which(&slay, "ay").is_none());
        let _ = std::fs::remove_dir_all(&sdir);

        // The pkg protocol, in its real shape (no tree_root), reaches its LANE — which,
        // with nothing at the provides path and no elevation on this thread, answers
        // Ok-with-deferred: the canonical `needs admin` state, no byte moved.
        let qdir = scratch("proto-pkg-real");
        let pkg_body = format!(
            "schema = 2\nprogram = \"ay\"\nversion = \"0.1\"\nbuild_number = 18\n\
             exposes = []\n\
             [[artifact]]\ntarget = \"{TRIPLE}\"\nkind = \"installer-pkg\"\nprotocol = \"pkg\"\n\
             url = \"https://github.com/Homebrew/brew/releases/download/4.5.0/Homebrew-4.5.0.pkg\"\n\
             sha256 = \"{}\"\nsize = 144434507\nsigner_team = \"927JGANW46\"\nelevated = true\n\
             provides = [\"/nope/opt/homebrew/bin/brew\"]\n",
            "7b09f01c".repeat(8)
        );
        let mut pkgs = HashMap::new();
        pkgs.insert(
            ("ay".to_string(), 18u64),
            (
                pkg_body.clone().into_bytes(),
                sign(&RELEASE_SEED, pkg_body.as_bytes()),
            ),
        );
        let fake = Fake {
            index: index_body.clone().into_bytes(),
            index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
            pkg: pkgs,
            archives: HashMap::new(),
        };
        let qlay = layout(&qdir);
        assert_eq!(
            crate::elevate::elevation(),
            crate::elevate::Elevation::Deferred
        );
        let report = install(&fake, &qlay, &anchor(), &req, fl(0), 0).unwrap();
        assert_eq!(
            report.protocol,
            Some(ProtocolOutcome::NeedsAdmin { protocol: "pkg" })
        );
        assert_eq!(
            report.protocol.as_ref().unwrap().state("ay"),
            crate::state::needs_admin("ay")
        );
        assert!(report.shimmed.is_empty() && report.tree_root.is_empty());
        assert!(!qlay.staging_dir("ay").exists(), "no byte moved");
        let _ = std::fs::remove_dir_all(&qdir);
    }

    // ---- the OS-installer lanes through the real flow ----

    /// A fetcher for the `pkg` lane: the signed index + manifests of `inner`, the
    /// package bytes served on `download_url` ONLY (the slug lane panics), every vendor
    /// request recorded.
    struct PkgFake {
        inner: Fake,
        served: PathBuf,
        seen: RefCell<Vec<(String, u64)>>,
    }
    impl Fetcher for PkgFake {
        fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
            self.inner.index_candidates()
        }
        fn pkg_manifest(&self, r: &str, p: &str, b: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
            self.inner.pkg_manifest(r, p, b)
        }
        fn download(&self, _: &str, asset: &str, _: &Path) -> Result<(), String> {
            panic!("a pkg row must never reach the slug lane (asked for {asset})");
        }
        fn download_url(&self, url: &str, dest: &Path, cap: u64) -> Result<(), String> {
            self.seen.borrow_mut().push((url.to_string(), cap));
            std::fs::copy(&self.served, dest)
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
    }

    const BREW_URL: &str = "https://github.com/Homebrew/brew/releases/download/6.0.20/Homebrew.pkg";

    /// The signed release for the `pkg` lane: `brew` pinned at 18 with a `pkg` row over
    /// the bytes at `<dir>/Homebrew.pkg` (signed sha256 = `signed_sha`, or the real one),
    /// `provides` as given; and, when `clt_row` is `Some`, a `clt` program at build 7
    /// carrying that `[[artifact]]` body, which `brew` REQUIRES through the index.
    fn fixture_pkg(
        dir: &Path,
        provides: &[String],
        signed_sha: Option<&str>,
        clt_row: Option<&str>,
    ) -> PkgFake {
        let served = dir.join("Homebrew.pkg");
        std::fs::write(&served, b"xar! not really a package, but signed bytes").unwrap();
        let real_sha = crate::tree::file_sha256(&served).unwrap();
        let sha = signed_sha.unwrap_or(&real_sha);
        let size = std::fs::metadata(&served).unwrap().len();
        let provides_toml: Vec<String> = provides.iter().map(|p| format!("{p:?}")).collect();
        let (requires, clt_prog, clt_pin) = if clt_row.is_some() {
            (
                "requires = [\"clt\"]\n",
                "[programs.clt]\nrepo = \"clt\"\n",
                ", clt = 7",
            )
        } else {
            ("", "", "")
        };
        let index_body = format!(
            "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
             [programs.brew]\nrepo = \"brew\"\n{requires}{clt_prog}\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             pin = {{ brew = 18{clt_pin} }}\n",
            attr = attribution()
        );
        let brew_body = format!(
            "schema = 2\nprogram = \"brew\"\nversion = \"6.0.20\"\nbuild_number = 18\n\
             exposes = []\n\
             [[artifact]]\ntarget = \"{TRIPLE}\"\nkind = \"installer-pkg\"\nprotocol = \"pkg\"\n\
             url = \"{BREW_URL}\"\nsha256 = \"{sha}\"\nsize = {size}\n\
             signer_team = \"927JGANW46\"\nelevated = true\n\
             provides = [{}]\nvendor = \"Homebrew\"\n",
            provides_toml.join(", ")
        );
        let mut pkg = HashMap::new();
        pkg.insert(
            ("brew".to_string(), 18u64),
            (
                brew_body.clone().into_bytes(),
                sign(&RELEASE_SEED, brew_body.as_bytes()),
            ),
        );
        if let Some(row) = clt_row {
            let clt_body = format!(
                "schema = 2\nprogram = \"clt\"\nversion = \"16.4\"\nbuild_number = 7\n\
                 exposes = []\n[[artifact]]\ntarget = \"{TRIPLE}\"\n{row}"
            );
            pkg.insert(
                ("clt".to_string(), 7u64),
                (
                    clt_body.clone().into_bytes(),
                    sign(&RELEASE_SEED, clt_body.as_bytes()),
                ),
            );
        }
        PkgFake {
            inner: Fake {
                index: index_body.clone().into_bytes(),
                index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
                pkg,
                archives: HashMap::new(),
            },
            served,
            seen: RefCell::new(Vec::new()),
        }
    }

    /// The Command Line Tools row, with `provides` as given.
    fn clt_row(provides: &str) -> String {
        format!(
            "kind = \"system-package\"\nprotocol = \"softwareupdate\"\n\
             label_prefix = \"Command Line Tools for Xcode\"\nelevated = true\n\
             provides = [{provides:?}]\nvendor = \"Apple\"\n"
        )
    }

    fn brew_req() -> InstallRequest<'static> {
        InstallRequest {
            channel: "stable",
            program: "brew",
            triple: TRIPLE,
            installed: None,
        }
    }

    /// Restore the thread's policy on every exit path of a test that raises it.
    struct Deferred;
    impl Drop for Deferred {
        fn drop(&mut self) {
            crate::elevate::set_elevation(crate::elevate::Elevation::Deferred);
        }
    }

    /// A `pkg` member whose `provides` path already exists is `installed via pkg:
    /// <path>` — no download, no installer, whatever the elevation; and one whose path
    /// is absent, under a Deferred policy, is `needs admin` with no byte moved and no
    /// tool run.
    #[test]
    fn a_pkg_member_is_proven_by_its_provides_path_and_defers_without_elevation() {
        let dir = scratch("pkg-provides");
        let present = dir.join("opt-homebrew-bin-brew");
        std::fs::write(&present, "brew").unwrap();
        let fake = fixture_pkg(
            &dir,
            &[
                String::from("/nope/brew"),
                present.to_string_lossy().into_owned(),
            ],
            None,
            None,
        );
        let lay = layout(&dir);
        // Even with sudo allowed, a present member runs nothing: the recorder would
        // answer garbage to any call, and the download lane would record one.
        let _restore = Deferred;
        crate::elevate::set_elevation(crate::elevate::Elevation::Sudo);
        let rec = Rc::new(crate::elevate::testkit::Recorder::new(vec![]));
        let report = crate::elevate::with_runner(rec.clone(), || {
            install(&fake, &lay, &anchor(), &brew_req(), fl(0), 0).unwrap()
        });
        assert_eq!(
            report.protocol,
            Some(ProtocolOutcome::Installed {
                protocol: "pkg",
                path: present.clone()
            })
        );
        assert_eq!(
            report.protocol.as_ref().unwrap().state("brew"),
            crate::state::installed_via("pkg", &present)
        );
        assert!(rec.argvs().is_empty(), "nothing ran");
        assert!(fake.seen.borrow().is_empty(), "nothing downloaded");
        assert!(!lay.staging_dir("brew").exists());
        assert!(
            crate::ops::which(&lay, "brew").is_none(),
            "no shim: the provides path IS the copy"
        );
        // Absent + Deferred: needs admin, nothing moved, nothing run.
        crate::elevate::set_elevation(crate::elevate::Elevation::Deferred);
        std::fs::remove_file(&present).unwrap();
        let report = crate::elevate::with_runner(rec.clone(), || {
            install(&fake, &lay, &anchor(), &brew_req(), fl(0), 0).unwrap()
        });
        assert_eq!(
            report.protocol,
            Some(ProtocolOutcome::NeedsAdmin { protocol: "pkg" })
        );
        assert!(rec.argvs().is_empty());
        assert!(
            fake.seen.borrow().is_empty(),
            "a deferred member downloads nothing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE pkg LANE, end to end under the terminal door: the package is downloaded
    /// through `download_url` ONLY (signed url, signed size as the cap) into
    /// `staging/brew/Homebrew.pkg`, gated on the signed sha256, checked with `pkgutil`
    /// (captured), applied by EXACTLY `sudo /usr/sbin/installer -pkg <file> -target /`
    /// (inherited), proven by the provides path the installer left, and the package is
    /// deleted afterwards. A wrong signer team never reaches the installer; a sha256
    /// mismatch never reaches pkgutil; both leave nothing behind.
    #[test]
    fn a_pkg_member_installs_through_the_signed_download_the_team_check_and_the_elevated_installer()
    {
        use crate::elevate::testkit::{Recorder, ok};
        use crate::installer_pkg::fixtures::{GOOD, WRONG_TEAM};
        let _restore = Deferred;
        let dir = scratch("pkg-lane");
        let brew = dir.join("opt-homebrew-bin-brew");
        let fake = fixture_pkg(&dir, &[brew.to_string_lossy().into_owned()], None, None);
        let lay = layout(&dir);
        let staged = lay.staging_dir("brew").join("Homebrew.pkg");
        crate::elevate::set_elevation(crate::elevate::Elevation::Sudo);
        let mut rec = Recorder::new(vec![ok(GOOD), ok("")]);
        let (created, expect_staged) = (brew.clone(), staged.clone());
        rec.on_run = Some(Box::new(move |argv: &[String]| {
            if argv.iter().any(|a| a == "/usr/sbin/installer") {
                assert!(
                    expect_staged.is_file(),
                    "the package is on disk while installer runs"
                );
                std::fs::write(&created, "brew").unwrap();
            }
        }));
        let rec = Rc::new(rec);
        let report = crate::elevate::with_runner(rec.clone(), || {
            install(&fake, &lay, &anchor(), &brew_req(), fl(0), 0).unwrap()
        });
        assert_eq!(
            report.protocol,
            Some(ProtocolOutcome::Installed {
                protocol: "pkg",
                path: brew.clone()
            })
        );
        assert_eq!(report.build, 18);
        assert!(report.shimmed.is_empty());
        let seen = fake.seen.borrow();
        assert_eq!(seen.len(), 1, "exactly one vendor download");
        assert_eq!(seen[0].0, BREW_URL, "the SIGNED url, verbatim");
        assert_eq!(
            seen[0].1,
            std::fs::metadata(&fake.served).unwrap().len(),
            "the cap is the signed size, exactly"
        );
        let calls = rec.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0],
            (
                crate::installer_pkg::check_signature_argv(&staged),
                crate::elevate::Io::Capture
            )
        );
        assert_eq!(
            calls[1],
            (
                vec![
                    "/usr/bin/sudo".to_string(),
                    "/usr/sbin/installer".to_string(),
                    "-pkg".to_string(),
                    staged.to_string_lossy().into_owned(),
                    "-target".to_string(),
                    "/".to_string(),
                ],
                crate::elevate::Io::Inherit
            )
        );
        assert!(!staged.exists(), "the package is deleted after the install");
        drop(calls);
        drop(seen);

        // Wrong team: refused by name, installer never run, package reclaimed.
        let wdir = scratch("pkg-wrong-team");
        let wfake = fixture_pkg(&wdir, &[String::from("/nope/brew")], None, None);
        let wlay = layout(&wdir);
        let wrec = Rc::new(Recorder::new(vec![ok(WRONG_TEAM), ok("")]));
        let err = crate::elevate::with_runner(wrec.clone(), || {
            install(&wfake, &wlay, &anchor(), &brew_req(), fl(0), 0).unwrap_err()
        });
        match &err {
            FlowError::Protocol {
                protocol: "pkg",
                why,
            } => {
                assert!(
                    why.contains("not the pinned signer_team 927JGANW46"),
                    "{why}"
                );
            }
            other => panic!("expected a pkg refusal, got {other:?}"),
        }
        assert!(
            err.to_string().starts_with("pkg: refusing to install"),
            "{err}"
        );
        assert_eq!(wrec.argvs().len(), 1, "pkgutil only");
        assert!(!wlay.staging_dir("brew").join("Homebrew.pkg").exists());

        // sha256 mismatch: the signed digest disagrees with the served bytes — refused
        // before pkgutil, nothing left in staging.
        let sdir = scratch("pkg-sha");
        let sfake = fixture_pkg(
            &sdir,
            &[String::from("/nope/brew")],
            Some(&"7b09f01c".repeat(8)),
            None,
        );
        let slay = layout(&sdir);
        let srec = Rc::new(Recorder::new(vec![ok(GOOD), ok("")]));
        let err = crate::elevate::with_runner(srec.clone(), || {
            install(&sfake, &slay, &anchor(), &brew_req(), fl(0), 0).unwrap_err()
        });
        assert!(
            matches!(err, FlowError::Stage(StageError::Sha256Mismatch { .. })),
            "{err:?}"
        );
        assert!(srec.argvs().is_empty(), "nothing ran on a bad download");
        assert!(!slay.staging_dir("brew").join("Homebrew.pkg").exists());
        for d in [dir, wdir, sdir] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// `requires` for an OS-installed member (Homebrew requires the Command Line Tools,
    /// through the INDEX's `[programs.brew].requires`): the dependency is resolved FIRST
    /// through the same flow; its deferral defers brew (both wait for the door); its
    /// presence lets brew proceed; its refusal stops brew with the reason.
    #[test]
    fn an_os_installed_member_waits_for_its_required_clt() {
        // clt absent + Deferred ⇒ clt defers ⇒ brew defers, without touching its own lane.
        let dir = scratch("pkg-requires-deferred");
        let fake = fixture_pkg(
            &dir,
            &[String::from("/nope/brew")],
            None,
            Some(&clt_row("/nope/CommandLineTools/usr/bin/git")),
        );
        let lay = layout(&dir);
        let report = install(&fake, &lay, &anchor(), &brew_req(), fl(0), 0).unwrap();
        assert_eq!(
            report.protocol,
            Some(ProtocolOutcome::NeedsAdmin { protocol: "pkg" })
        );
        assert_eq!(report.dependencies.len(), 1);
        assert_eq!(report.dependencies[0].program, "clt");
        assert_eq!(
            report.dependencies[0].result,
            DepResult::Protocol(ProtocolOutcome::NeedsAdmin {
                protocol: "softwareupdate"
            })
        );
        assert!(
            fake.seen.borrow().is_empty(),
            "brew's own lane never started"
        );
        let _ = std::fs::remove_dir_all(&dir);

        // clt present ⇒ recorded `installed via softwareupdate` beside brew's own answer.
        let pdir = scratch("pkg-requires-present");
        let git = pdir.join("CommandLineTools-usr-bin-git");
        std::fs::write(&git, "git").unwrap();
        let pfake = fixture_pkg(
            &pdir,
            &[String::from("/nope/brew")],
            None,
            Some(&clt_row(&git.to_string_lossy())),
        );
        let play = layout(&pdir);
        let report = install(&pfake, &play, &anchor(), &brew_req(), fl(0), 0).unwrap();
        assert_eq!(
            report.dependencies[0].result,
            DepResult::Protocol(ProtocolOutcome::Installed {
                protocol: "softwareupdate",
                path: git.clone()
            })
        );
        assert_eq!(
            report.protocol,
            Some(ProtocolOutcome::NeedsAdmin { protocol: "pkg" }),
            "brew itself still waits for the door on this pass"
        );
        let _ = std::fs::remove_dir_all(&pdir);

        // clt refused (its row fails admission) ⇒ brew stops with the requirement named.
        let rdir = scratch("pkg-requires-refused");
        let rfake = fixture_pkg(
            &rdir,
            &[String::from("/nope/brew")],
            None,
            Some(
                "kind = \"system-package\"\nprotocol = \"softwareupdate\"\nelevated = true\n\
                 provides = [\"/nope/git\"]\n",
            ),
        );
        let rlay = layout(&rdir);
        let err = install(&rfake, &rlay, &anchor(), &brew_req(), fl(0), 0).unwrap_err();
        match &err {
            FlowError::Requirement { dep, why } => {
                assert_eq!(dep, "clt");
                assert!(why.contains("label_prefix must be"), "{why}");
            }
            other => panic!("expected Requirement, got {other:?}"),
        }
        assert!(
            err.to_string()
                .starts_with("requires clt, which could not be installed first: "),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&rdir);
    }

    /// An OS-installed member's requirement gate counts a dependency that is MET, however
    /// this call reached it. A DIAMOND — brew requires [mid, clt], mid requires clt — has
    /// clt resolved inside mid's recursion before brew's own clt edge is walked; that edge
    /// carries clt's real outcome, so brew proceeds (it used to stop with `requires clt,
    /// which could not be installed first: already resolved or cycle`). The same holds
    /// across subtrees (top requires [mid, brew]), and a DEV-LINKED dependency is met
    /// exactly as `requires::unmet_requirement` counts it.
    #[test]
    fn an_os_installed_member_counts_a_requirement_met_earlier_or_dev_linked() {
        let dir = scratch("pkg-requires-diamond");
        let git = dir.join("CommandLineTools-usr-bin-git");
        let mid_bin = dir.join("mid-bin");
        let top_bin = dir.join("top-bin");
        for p in [&git, &mid_bin, &top_bin] {
            std::fs::write(p, "present").unwrap();
        }
        let base = fixture_pkg(
            &dir,
            &[String::from("/nope/brew")],
            None,
            Some(&clt_row(&git.to_string_lossy())),
        );
        let index_body = String::from_utf8(base.inner.index.clone())
            .unwrap()
            .replace(
                "[programs.brew]\nrepo = \"brew\"\nrequires = [\"clt\"]\n",
                "[programs.brew]\nrepo = \"brew\"\nrequires = [\"mid\", \"clt\"]\n\
                 [programs.mid]\nrepo = \"mid\"\nrequires = [\"clt\"]\n\
                 [programs.top]\nrepo = \"top\"\nrequires = [\"mid\", \"brew\"]\n",
            )
            .replace(", clt = 7", ", clt = 7, mid = 3, top = 5");
        assert!(index_body.contains("top = 5"), "{index_body}");
        let mut pkg = base.inner.pkg;
        for (name, build, bin) in [("mid", 3u64, &mid_bin), ("top", 5, &top_bin)] {
            let body = format!(
                "schema = 2\nprogram = \"{name}\"\nversion = \"1.0\"\nbuild_number = {build}\n\
                 exposes = []\n[[artifact]]\ntarget = \"{TRIPLE}\"\n{}",
                clt_row(&bin.to_string_lossy())
            );
            pkg.insert(
                (name.to_string(), build),
                (
                    body.clone().into_bytes(),
                    sign(&RELEASE_SEED, body.as_bytes()),
                ),
            );
        }
        let fake = PkgFake {
            inner: Fake {
                index: index_body.clone().into_bytes(),
                index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
                pkg,
                archives: HashMap::new(),
            },
            served: base.served,
            seen: RefCell::new(Vec::new()),
        };
        let lay = layout(&dir);

        // The diamond: brew passes its gate and waits for the door; clt is named ONCE.
        let report = install(&fake, &lay, &anchor(), &brew_req(), fl(0), 0).unwrap();
        assert_eq!(
            report.protocol,
            Some(ProtocolOutcome::NeedsAdmin { protocol: "pkg" })
        );
        assert_eq!(
            report.dependencies,
            vec![
                DepOutcome {
                    program: "mid".into(),
                    result: DepResult::Protocol(ProtocolOutcome::Installed {
                        protocol: "softwareupdate",
                        path: mid_bin.clone(),
                    }),
                },
                DepOutcome {
                    program: "clt".into(),
                    result: DepResult::Protocol(ProtocolOutcome::Installed {
                        protocol: "softwareupdate",
                        path: git.clone(),
                    }),
                },
            ]
        );

        // Across subtrees: brew (reached through top) still sees clt as met, so top
        // defers on brew's deferral instead of failing on a skipped brew.
        let top = InstallRequest {
            program: "top",
            ..brew_req()
        };
        let report = install(&fake, &lay, &anchor(), &top, fl(0), 0).unwrap();
        assert_eq!(
            report.protocol,
            Some(ProtocolOutcome::NeedsAdmin {
                protocol: "softwareupdate"
            })
        );
        assert!(
            report.dependencies.iter().any(|d| d.program == "brew"
                && d.result
                    == DepResult::Protocol(ProtocolOutcome::NeedsAdmin { protocol: "pkg" })),
            "{:?}",
            report.dependencies
        );
        assert!(
            report
                .dependencies
                .iter()
                .all(|d| !matches!(d.result, DepResult::Skipped(_))),
            "{:?}",
            report.dependencies
        );
        assert!(fake.seen.borrow().is_empty(), "no vendor download started");
        let _ = std::fs::remove_dir_all(&dir);

        // A dev-linked clt (absent from its provides path) is met: brew proceeds.
        let ldir = scratch("pkg-requires-linked");
        let lfake = fixture_pkg(
            &ldir,
            &[String::from("/nope/brew")],
            None,
            Some(&clt_row("/nope/CommandLineTools/usr/bin/git")),
        );
        let llay = layout(&ldir);
        mark_linked(&llay, "clt");
        let report = install(&lfake, &llay, &anchor(), &brew_req(), fl(0), 0).unwrap();
        assert_eq!(
            report.protocol,
            Some(ProtocolOutcome::NeedsAdmin { protocol: "pkg" })
        );
        assert!(
            matches!(&report.dependencies[..], [d] if d.program == "clt"
                && matches!(&d.result, DepResult::Skipped(why) if why.contains("dev-linked"))),
            "{:?}",
            report.dependencies
        );
        let _ = std::fs::remove_dir_all(&ldir);
    }

    /// THE EXPLICIT DOOR'S ORDER (§17.10): `aterm pkg install brew` under the terminal
    /// door (Sudo on this thread) installs the Command Line Tools FIRST — `softwareupdate
    /// -l`, then `sudo softwareupdate -i <label>` — and only then runs brew's own lane
    /// (`pkgutil --check-signature`, `sudo installer …`); one thread, one elevation
    /// policy, one sudo session. Every tool runs through the injected runner, which
    /// leaves each lane's `provides` path behind exactly as the real tools would.
    #[test]
    fn the_explicit_door_installs_a_required_clt_before_brew_in_one_session() {
        use crate::elevate::testkit::{Recorder, ok};
        use crate::installer_pkg::fixtures::GOOD;
        let _restore = Deferred;
        let dir = scratch("door-order");
        let git = dir.join("CommandLineTools-usr-bin-git");
        let brew_bin = dir.join("opt-homebrew-bin-brew");
        let fake = fixture_pkg(
            &dir,
            &[brew_bin.to_string_lossy().into_owned()],
            None,
            Some(&clt_row(&git.to_string_lossy())),
        );
        let lay = layout(&dir);
        crate::elevate::set_elevation(crate::elevate::Elevation::Sudo);
        let mut rec = Recorder::new(vec![
            ok(
                "Software Update found the following new or updated software:\n\
                * Label: Command Line Tools for Xcode-16.4\n",
            ),
            ok(""),
            ok(GOOD),
            ok(""),
        ]);
        let (git_c, brew_c) = (git.clone(), brew_bin.clone());
        rec.on_run = Some(Box::new(move |argv: &[String]| {
            // Each ELEVATED install leaves its proof behind; the listing and the
            // signature check leave nothing.
            if argv.first().map(String::as_str) == Some("/usr/bin/sudo") {
                if argv.iter().any(|a| a == "/usr/sbin/softwareupdate") {
                    std::fs::write(&git_c, "git").unwrap();
                } else {
                    std::fs::write(&brew_c, "brew").unwrap();
                }
            }
        }));
        let rec = Rc::new(rec);
        let report = crate::elevate::with_runner(rec.clone(), || {
            install(&fake, &lay, &anchor(), &brew_req(), fl(0), 0).unwrap()
        });
        // brew installed, clt installed first, both proven.
        assert_eq!(
            report.protocol,
            Some(ProtocolOutcome::Installed {
                protocol: "pkg",
                path: brew_bin.clone()
            })
        );
        assert_eq!(report.dependencies.len(), 1);
        assert_eq!(
            report.dependencies[0],
            DepOutcome {
                program: "clt".into(),
                result: DepResult::Protocol(ProtocolOutcome::Installed {
                    protocol: "softwareupdate",
                    path: git.clone()
                })
            }
        );
        // THE ORDER, tool by tool: clt's two calls strictly before brew's two.
        let argvs = rec.argvs();
        assert_eq!(argvs.len(), 4, "{argvs:?}");
        assert_eq!(argvs[0], vec!["/usr/sbin/softwareupdate", "-l"]);
        assert_eq!(
            argvs[1],
            vec![
                "/usr/bin/sudo",
                "/usr/sbin/softwareupdate",
                "-i",
                "Command Line Tools for Xcode-16.4"
            ]
        );
        assert_eq!(argvs[2][0], "/usr/sbin/pkgutil");
        assert_eq!(argvs[3][0], "/usr/bin/sudo");
        assert_eq!(argvs[3][1], "/usr/sbin/installer");
        assert!(
            !Path::new(crate::softwareupdate::PLACEHOLDER).exists(),
            "the on-demand placeholder is removed on every path"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Rule 2 on the `requires` path: a dependency the user's OWN copy satisfies
    /// (`system = "<bin>"`, the binary on PATH outside the prefix) is reported
    /// `DepResult::System(path)` and never installed — no manifest fetched, no byte
    /// moved, no managed copy laid beside the user's.
    #[cfg(unix)]
    #[test]
    fn a_system_satisfied_dependency_is_never_pulled_in_as_a_managed_copy() {
        let dir = scratch("requires-system");
        let sys = dir.join("usr-local-bin");
        let gh = lay_pm_exe(&sys, "gh");
        let path = std::env::join_paths([sys.clone()]).unwrap();
        // `ay` requires `gh`; gh declares `system = "gh"` and is pinned, but its pkg
        // manifest is deliberately ABSENT: fetching it would fail loudly.
        let ay_dir = dir.join("ay");
        std::fs::create_dir_all(&ay_dir).unwrap();
        let fake = requires_fixture(&ay_dir, &[], &["gh"], &[]);
        let index_body = String::from_utf8(fake.index.clone()).unwrap().replace(
            "[programs.ny]\nrepo = \"ny\"\n",
            "[programs.ny]\nrepo = \"ny\"\n[programs.gh]\nrepo = \"gh\"\nsystem = \"gh\"\n",
        );
        let index_body = index_body.replace("ny = 9 }", "ny = 9, gh = 3 }");
        assert!(index_body.contains("gh = 3"), "{index_body}");
        let fake = Fake {
            index: index_body.clone().into_bytes(),
            index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
            pkg: fake.pkg,
            archives: fake.archives,
        };
        let lay = layout(&dir);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let report = crate::elevate::with_path_var(Some(&path), || {
            install(&fake, &lay, &anchor(), &req, fl(0), 0).unwrap()
        });
        assert_eq!(report.build, 18);
        assert_eq!(
            report.dependencies,
            vec![DepOutcome {
                program: "gh".into(),
                result: DepResult::System(gh.clone())
            }]
        );
        assert!(
            !crate::ops::active_builds(&lay).contains_key("gh"),
            "no managed gh was laid beside the user's copy"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- the system-pm lane through the real flow ----

    /// The signed release for the `system-pm` lane: `emacs` pinned at 18 with the
    /// `[[artifact]]` body `row` for this triple (no `system = "emacs"` — the lane is
    /// under test, not the satisfaction reconcile), and
    /// `[programs.emacs].unavailable_hint` when `hint` is given.
    fn fixture_pm(dir: &Path, row: &str, hint: Option<&str>) -> Fake {
        let _ = dir;
        let hint_line = hint.map_or_else(String::new, |h| format!("unavailable_hint = {h:?}\n"));
        let index_body = format!(
            "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
             [programs.emacs]\nrepo = \"emacs\"\n{hint_line}\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             pin = {{ emacs = 18 }}\n",
            attr = attribution()
        );
        let pkg_body = format!(
            "schema = 2\nprogram = \"emacs\"\nversion = \"31.1\"\nbuild_number = 18\n\
             exposes = []\n[[artifact]]\ntarget = \"{TRIPLE}\"\n{row}"
        );
        let mut pkgs = HashMap::new();
        pkgs.insert(
            ("emacs".to_string(), 18u64),
            (
                pkg_body.clone().into_bytes(),
                sign(&RELEASE_SEED, pkg_body.as_bytes()),
            ),
        );
        Fake {
            index: index_body.clone().into_bytes(),
            index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
            pkg: pkgs,
            archives: HashMap::new(),
        }
    }

    /// A `system-pm` row over `manager`, `provides` as given, `elevated` as given.
    fn pm_row(manager: &str, package: &str, provides: &[&str], elevated: bool) -> String {
        let p: Vec<String> = provides.iter().map(|x| format!("{x:?}")).collect();
        format!(
            "kind = \"system-package\"\nprotocol = \"system-pm\"\nmanager = {manager:?}\n\
             package = {package:?}\nelevated = {elevated}\nprovides = [{}]\n",
            p.join(", ")
        )
    }

    fn emacs_req() -> InstallRequest<'static> {
        InstallRequest {
            channel: "stable",
            program: "emacs",
            triple: TRIPLE,
            installed: None,
        }
    }

    #[cfg(unix)]
    fn lay_pm_exe(dir: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// A `system-pm` member whose `provides` already resolves — a bare name on PATH, or
    /// an absolute path — is `installed via <manager>: <path>`: no manager is looked up,
    /// nothing runs, whatever the policy. The manager's NAME is the state's protocol.
    #[cfg(unix)]
    #[test]
    fn a_system_pm_member_is_proven_by_its_provides_before_any_manager_runs() {
        use crate::elevate::testkit::Recorder;
        let dir = scratch("syspm-provides");
        let sys = dir.join("usr-bin");
        let emacs = lay_pm_exe(&sys, "emacs");
        let path = std::env::join_paths([sys.clone()]).unwrap();
        let fake = fixture_pm(&dir, &pm_row("apt", "emacs", &["emacs"], true), None);
        let lay = layout(&dir);
        let rec = Rc::new(Recorder::new(vec![]));
        let report = crate::elevate::with_path_var(Some(&path), || {
            crate::elevate::with_runner(rec.clone(), || {
                install(&fake, &lay, &anchor(), &emacs_req(), fl(0), 0).unwrap()
            })
        });
        assert_eq!(
            report.protocol,
            Some(ProtocolOutcome::Installed {
                protocol: "apt",
                path: emacs.clone()
            })
        );
        assert_eq!(
            report.protocol.as_ref().unwrap().state("emacs"),
            crate::state::installed_via("apt", &emacs)
        );
        assert!(rec.argvs().is_empty(), "nothing ran");
        assert!(report.shimmed.is_empty() && report.tree_root.is_empty());
        // An absolute proof, with no PATH at all.
        let abs = dir.join("opt-emacs");
        std::fs::write(&abs, "emacs").unwrap();
        let afake = fixture_pm(
            &dir,
            &pm_row(
                "apt",
                "emacs",
                &["/nope/emacs", &abs.to_string_lossy()],
                true,
            ),
            None,
        );
        let report = crate::elevate::with_path_var(None, || {
            crate::elevate::with_runner(rec.clone(), || {
                install(&afake, &lay, &anchor(), &emacs_req(), fl(0), 0).unwrap()
            })
        });
        assert_eq!(
            report.protocol,
            Some(ProtocolOutcome::Installed {
                protocol: "apt",
                path: abs
            })
        );
        assert!(rec.argvs().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `system-pm` member whose manager is not on PATH is `unavailable on <target>:
    /// <hint>` — the index author's hint first, then which manager is missing; not
    /// deferred (nothing waits on the door for a manager atpkg never installs), nothing
    /// run. An unset PATH is the same answer.
    #[test]
    fn a_system_pm_member_without_its_manager_is_unavailable_here() {
        use crate::elevate::testkit::Recorder;
        let dir = scratch("syspm-no-manager");
        let empty = std::ffi::OsString::new();
        let fake = fixture_pm(
            &dir,
            &pm_row("apt", "emacs", &["emacs"], true),
            Some("Emacs is a macOS/Linux member"),
        );
        let lay = layout(&dir);
        let rec = Rc::new(Recorder::new(vec![]));
        for path in [Some(empty.as_os_str()), None] {
            let report = crate::elevate::with_path_var(path, || {
                crate::elevate::with_runner(rec.clone(), || {
                    install(&fake, &lay, &anchor(), &emacs_req(), fl(0), 0).unwrap()
                })
            });
            let outcome = report.protocol.clone().unwrap();
            assert!(outcome.is_unavailable() && !outcome.is_deferred());
            assert_eq!(outcome.protocol(), "apt");
            assert_eq!(
                outcome.state("emacs"),
                "unavailable on aarch64-apple-darwin: Emacs is a macOS/Linux member; apt is \
                 not on PATH (the pinned row installs emacs through it, and atpkg never \
                 installs a package manager)"
            );
            assert!(rec.argvs().is_empty(), "nothing ran");
        }
        // Without an index hint, the fact alone.
        let plain = fixture_pm(&dir, &pm_row("apt", "emacs", &["emacs"], true), None);
        let report = crate::elevate::with_path_var(Some(&empty), || {
            install(&plain, &lay, &anchor(), &emacs_req(), fl(0), 0).unwrap()
        });
        assert!(
            report
                .protocol
                .unwrap()
                .state("emacs")
                .starts_with("unavailable on aarch64-apple-darwin: apt is not on PATH ("),
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A SYSTEM-WIDE manager on PATH: the unattended pass DEFERS (`needs admin`, nothing
    /// runs); the terminal door runs EXACTLY `sudo <apt-get> install -y emacs`
    /// (inherited) and proves the install by the bare name apt laid on PATH; a manager
    /// that fails is a `Protocol` error naming the manager and the exit.
    #[cfg(unix)]
    #[test]
    fn a_system_wide_manager_defers_unattended_and_installs_under_sudo_at_the_door() {
        use crate::elevate::testkit::{Recorder, failed, ok};
        let _restore = Deferred;
        let dir = scratch("syspm-apt");
        let sys = dir.join("usr-bin");
        let apt_get = lay_pm_exe(&sys, "apt-get");
        let path = std::env::join_paths([sys.clone()]).unwrap();
        let fake = fixture_pm(&dir, &pm_row("apt", "emacs", &["emacs"], true), None);
        let lay = layout(&dir);
        // Deferred: needs admin, nothing runs.
        let rec = Rc::new(Recorder::new(vec![ok("")]));
        let report = crate::elevate::with_path_var(Some(&path), || {
            crate::elevate::with_runner(rec.clone(), || {
                install(&fake, &lay, &anchor(), &emacs_req(), fl(0), 0).unwrap()
            })
        });
        assert_eq!(
            report.protocol,
            Some(ProtocolOutcome::NeedsAdmin { protocol: "apt" })
        );
        assert_eq!(
            report.protocol.as_ref().unwrap().state("emacs"),
            crate::state::needs_admin("emacs")
        );
        assert!(
            rec.argvs().is_empty(),
            "the unattended pass runs no manager"
        );
        // The door: sudo, exact argv, proven.
        crate::elevate::set_elevation(crate::elevate::Elevation::Sudo);
        let mut rec = Recorder::new(vec![ok("")]);
        let created = sys.clone();
        rec.on_run = Some(Box::new(move |argv: &[String]| {
            assert_eq!(argv[0], "/usr/bin/sudo");
            lay_pm_exe(&created, "emacs");
        }));
        let rec = Rc::new(rec);
        let report = crate::elevate::with_path_var(Some(&path), || {
            crate::elevate::with_runner(rec.clone(), || {
                install(&fake, &lay, &anchor(), &emacs_req(), fl(0), 0).unwrap()
            })
        });
        assert_eq!(
            report.protocol,
            Some(ProtocolOutcome::Installed {
                protocol: "apt",
                path: sys.join("emacs")
            })
        );
        assert_eq!(
            rec.calls.borrow()[..],
            [(
                vec![
                    "/usr/bin/sudo".to_string(),
                    apt_get.to_string_lossy().into_owned(),
                    "install".to_string(),
                    "-y".to_string(),
                    "emacs".to_string(),
                ],
                crate::elevate::Io::Inherit
            )]
        );
        assert!(!lay.staging_dir("emacs").exists(), "no byte of ours moved");
        // The manager failing.
        let _ = std::fs::remove_file(sys.join("emacs"));
        let rec = Rc::new(Recorder::new(vec![failed(
            100,
            "E: Unable to locate package",
        )]));
        let err = crate::elevate::with_path_var(Some(&path), || {
            crate::elevate::with_runner(rec.clone(), || {
                install(&fake, &lay, &anchor(), &emacs_req(), fl(0), 0).unwrap_err()
            })
        });
        match &err {
            FlowError::Protocol {
                protocol: "apt",
                why,
            } => assert!(
                why.starts_with("apt install emacs failed (exit 100)"),
                "{why}"
            ),
            other => panic!("expected an apt failure, got {other:?}"),
        }
        assert!(
            err.to_string().starts_with("apt: apt install emacs failed"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A USER-SCOPED manager (brew) runs UNATTENDED and UNWRAPPED: a row that declares
    /// no elevation needs no one's password, so the default (Deferred) policy runs it as
    /// the user; a winget row that declares `elevated = true` (a machine-scoped
    /// installer) defers unattended and, at the door, runs unwrapped with
    /// `--scope machine`.
    #[cfg(unix)]
    #[test]
    fn a_user_scoped_manager_runs_unattended_and_unwrapped() {
        use crate::elevate::testkit::{Recorder, ok};
        let _restore = Deferred;
        let dir = scratch("syspm-brew");
        let bin = dir.join("opt-homebrew-bin");
        let brew = lay_pm_exe(&bin, "brew");
        let path = std::env::join_paths([bin.clone()]).unwrap();
        let fake = fixture_pm(&dir, &pm_row("brew", "emacs", &["emacs"], false), None);
        let lay = layout(&dir);
        let mut rec = Recorder::new(vec![ok("")]);
        let created = bin.clone();
        rec.on_run = Some(Box::new(move |_argv: &[String]| {
            lay_pm_exe(&created, "emacs");
        }));
        let rec = Rc::new(rec);
        assert_eq!(
            crate::elevate::elevation(),
            crate::elevate::Elevation::Deferred
        );
        let report = crate::elevate::with_path_var(Some(&path), || {
            crate::elevate::with_runner(rec.clone(), || {
                install(&fake, &lay, &anchor(), &emacs_req(), fl(0), 0).unwrap()
            })
        });
        assert_eq!(
            report.protocol,
            Some(ProtocolOutcome::Installed {
                protocol: "brew",
                path: bin.join("emacs")
            })
        );
        assert_eq!(
            rec.argvs(),
            vec![vec![
                brew.to_string_lossy().into_owned(),
                "install".to_string(),
                "emacs".to_string()
            ]],
            "brew runs as the user, unattended, never under sudo"
        );
        // winget, elevated: deferred unattended; at the door, unwrapped, machine scope.
        let _ = std::fs::remove_file(bin.join("emacs"));
        let winget = lay_pm_exe(&bin, "winget");
        let wfake = fixture_pm(&dir, &pm_row("winget", "GNU.Emacs", &["emacs"], true), None);
        let rec = Rc::new(Recorder::new(vec![ok("")]));
        let report = crate::elevate::with_path_var(Some(&path), || {
            crate::elevate::with_runner(rec.clone(), || {
                install(&wfake, &lay, &anchor(), &emacs_req(), fl(0), 0).unwrap()
            })
        });
        assert_eq!(
            report.protocol,
            Some(ProtocolOutcome::NeedsAdmin { protocol: "winget" })
        );
        assert!(rec.argvs().is_empty());
        crate::elevate::set_elevation(crate::elevate::Elevation::Sudo);
        let mut rec = Recorder::new(vec![ok("")]);
        let created = bin.clone();
        rec.on_run = Some(Box::new(move |_argv: &[String]| {
            lay_pm_exe(&created, "emacs");
        }));
        let rec = Rc::new(rec);
        let report = crate::elevate::with_path_var(Some(&path), || {
            crate::elevate::with_runner(rec.clone(), || {
                install(&wfake, &lay, &anchor(), &emacs_req(), fl(0), 0).unwrap()
            })
        });
        assert_eq!(
            report.protocol,
            Some(ProtocolOutcome::Installed {
                protocol: "winget",
                path: bin.join("emacs")
            })
        );
        let argvs = rec.argvs();
        assert_eq!(argvs.len(), 1);
        assert_eq!(argvs[0][0], winget.to_string_lossy(), "unwrapped: no sudo");
        assert_eq!(
            &argvs[0][1..],
            &[
                "install",
                "--exact",
                "--id",
                "GNU.Emacs",
                "--accept-package-agreements",
                "--accept-source-agreements",
                "--scope",
                "machine"
            ]
            .map(String::from)[..]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A requirement that is UNAVAILABLE here (a `system-pm` dependency whose manager is
    /// absent) stops an OS-installed parent with the reason — it neither waits on the
    /// door (nothing there would install the manager) nor reaches the parent's lane.
    #[test]
    fn an_unavailable_requirement_stops_an_os_installed_parent() {
        let dir = scratch("pkg-requires-unavailable");
        let fake = fixture_pkg(
            &dir,
            &[String::from("/nope/brew")],
            None,
            Some(&pm_row("apt", "clt-tools", &["/nope/git"], true)),
        );
        let lay = layout(&dir);
        let empty = std::ffi::OsString::new();
        let err = crate::elevate::with_path_var(Some(&empty), || {
            install(&fake, &lay, &anchor(), &brew_req(), fl(0), 0).unwrap_err()
        });
        match &err {
            FlowError::Requirement { dep, why } => {
                assert_eq!(dep, "clt");
                assert!(
                    why.starts_with("unavailable on aarch64-apple-darwin: apt is not on PATH"),
                    "{why}"
                );
            }
            other => panic!("expected Requirement, got {other:?}"),
        }
        assert!(
            fake.seen.borrow().is_empty(),
            "brew's own lane never started"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `pkg` row's local staging name: its `asset` when it names one, else the URL's
    /// last path component, else `<program>.pkg` — never a path.
    #[test]
    fn a_pkg_row_gets_a_local_staging_name() {
        let mut a = crate::vendor::testkit::pkg_row();
        a.asset = String::new();
        a.url = BREW_URL.into();
        assert_eq!(local_asset_name("brew", &a), "Homebrew.pkg");
        a.url =
            "https://github.com/Homebrew/brew/releases/download/6.0.20/Homebrew.pkg?x=1#f".into();
        assert_eq!(local_asset_name("brew", &a), "Homebrew.pkg");
        a.url = "https://github.com/".into();
        assert_eq!(local_asset_name("brew", &a), "brew.pkg");
        a.asset = "Homebrew-6.0.20.pkg".into();
        assert_eq!(local_asset_name("brew", &a), "Homebrew-6.0.20.pkg");
        let https = crate::vendor::testkit::row();
        assert_eq!(local_asset_name("claude", &https), https.asset);
    }

    #[test]
    fn app_bundle_refused_closed_but_sysroot_bundle_installs() {
        // app-bundle is the app's own in-session apply topology, NOT a tool install — it
        // stays refused CLOSED on this path (atpkg never swaps the app).
        let adir = scratch("dispatch-app");
        let app = fixture_with_kind(&adir, "app-bundle");
        let alay = layout(&adir);
        let areq = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let err = install(&app, &alay, &anchor(), &areq, fl(0), 0).unwrap_err();
        // Now routed through the two-anchor app-apply gate (appgate::app_apply_allowed), which
        // fails closed on the CLI path because notarization is unproven here — a distinct,
        // gate-driven refusal rather than a blanket UnsupportedKind.
        assert!(
            matches!(err, FlowError::AppBundleRefused(ref p) if p == "ay"),
            "got {err:?}"
        );
        assert!(
            crate::ops::which(&alay, "ay").is_none(),
            "refused install leaves no shim"
        );
        let _ = std::fs::remove_dir_all(&adir);

        // sysroot-bundle (self-contained default: pack-time-relocated, no rustup
        // needed) is NO LONGER refused — it stages, wires (no-op for self-contained),
        // activates + shims, and passes the fail-loud resolve check (the fixture bin
        // `#!/bin/true` runs to a clean exit, proving the loader resolved it).
        let sdir = scratch("dispatch-sr");
        let sr = fixture_with_kind(&sdir, "sysroot-bundle");
        let slay = layout(&sdir);
        let sreq = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let rep = install(&sr, &slay, &anchor(), &sreq, fl(0), 0)
            .expect("self-contained sysroot-bundle should install");
        assert!(
            rep.shimmed.contains(&"ay".to_string()),
            "exposed tool shimmed"
        );
        assert_eq!(
            crate::ops::which(&slay, "ay").unwrap(),
            tool_bin(&slay.build_dir("ay", 18), "ay")
        );
        assert_eq!(
            std::fs::read_link(slay.channel_current("stable")).unwrap(),
            slay.build_dir("ay", 18)
        );
        let _ = std::fs::remove_dir_all(&sdir);
    }

    /// Mach-O magic over garbage, shipped with tar mode 0644: the magic makes
    /// [`crate::relocate::is_native_object`] admit it to the resolve check, and the
    /// missing exec bit makes the spawn fail (`EACCES`) — [`crate::sysroot::resolve_check`]'s
    /// documented "cannot spawn" arm — so [`bundle_resolve_check`] errors. (A garbage
    /// EXECUTABLE Mach-O is no good as a fixture: macOS reports the exec-format failure
    /// as a NORMAL exit 126, which the check's run-to-completion contract accepts.)
    const BROKEN_NATIVE_BIN: &[u8] = &[0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0];
    const NO_EXEC_MODE: &[u8; 8] = b"0000644\0";

    // THE resolve-failure unwind (fresh install): a sysroot-bundle whose exposed binary
    // cannot load must NOT be left ACTIVE — before the fix the channel `current`, the
    // witness, and the shims all kept naming the broken `.ready` build, `decide` read it
    // as UpToDate, and a retried install printed 'already current' forever. The install
    // path now unwinds like the transactional flip: no pointer survives, the build is
    // discarded, and a retry with a healthy bundle re-installs cleanly.
    #[cfg(unix)]
    #[test]
    fn failed_bundle_resolve_check_unwinds_a_fresh_install() {
        let dir = scratch("resolve-fresh");
        let broken = fixture_with(&dir, "sysroot-bundle", BROKEN_NATIVE_BIN, NO_EXEC_MODE);
        let layout = layout(&dir);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let err = install(&broken, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(matches!(err, FlowError::Activate(_)), "got {err:?}");
        // Nothing points at the broken build, and the build itself is gone…
        assert!(crate::ops::which(&layout, "ay").is_none(), "no live shim");
        assert!(
            std::fs::symlink_metadata(layout.program_current("ay")).is_err(),
            "no witness link"
        );
        assert!(
            std::fs::symlink_metadata(layout.channel_current("stable")).is_err(),
            "no channel link"
        );
        assert!(
            !layout.build_dir("ay", 18).exists(),
            "the broken build is discarded, never re-read as complete"
        );
        assert!(crate::ops::active_builds(&layout).is_empty());
        // …so a retry (healthy bundle, SAME store) re-installs instead of 'already current'.
        let hdir = scratch("resolve-fresh-retry");
        let healthy = fixture_with_kind(&hdir, "sysroot-bundle");
        let rep = install(&healthy, &layout, &anchor(), &req, fl(0), 0)
            .expect("a retry after the unwind re-installs");
        assert!(
            !rep.already_current,
            "the unwound install must not read as current"
        );
        assert_eq!(
            crate::ops::which(&layout, "ay").unwrap(),
            tool_bin(&layout.build_dir("ay", 18), "ay")
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&hdir);
    }

    // The upgrade variant of the resolve-failure unwind: ay@17 is live, the pinned
    // ay@18 bundle fails its resolve check — shims, witness, and channel `current` all
    // revert to 17 (the prior working surface) and 18 is discarded, exactly what
    // `flip_member` guarantees on the transactional path.
    #[cfg(unix)]
    #[test]
    fn failed_bundle_resolve_check_reverts_an_upgrade_to_the_prior_build() {
        let dir = scratch("resolve-revert");
        let layout = layout(&dir);
        seed_build(&layout, "ay", 17, true); // ay@17 active + shimmed
        let broken = fixture_with(&dir, "sysroot-bundle", BROKEN_NATIVE_BIN, NO_EXEC_MODE);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: Some(17),
        };
        let err = install(&broken, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(matches!(err, FlowError::Activate(_)), "got {err:?}");
        let b17 = layout.build_dir("ay", 17);
        assert_eq!(
            crate::ops::which(&layout, "ay").unwrap(),
            tool_bin(&b17, "ay"),
            "the shim reverts to the prior working build"
        );
        assert_eq!(
            std::fs::read_link(layout.program_current("ay")).unwrap(),
            b17,
            "the witness reverts"
        );
        assert_eq!(
            std::fs::read_link(layout.channel_current("stable")).unwrap(),
            b17,
            "the channel current reverts"
        );
        assert!(
            !layout.build_dir("ay", 18).exists(),
            "the broken build is discarded"
        );
        assert_eq!(
            crate::ops::active_builds(&layout).get("ay").copied(),
            Some(17),
            "the gate keeps seeing 17, so the update stays due — never 'already current'"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // §7 (step 21): the single-program `install` path also DISABLES a tombstoned build's old
    // working shim. ay@17 is active + shimmed; the channel pins ay=18 but yanks it, so decide()
    // tombstones — install must both return Tombstoned AND replace bin/ay with a failing shim.
    #[test]
    fn install_tombstone_disables_the_old_shim() {
        let dir = scratch("install-tomb");
        let layout = layout(&dir);
        seed_build(&layout, "ay", 17, true); // ay@17 active + shimmed
        // A LIVE forwarding shim (a symlink on Unix, a forwarding `.cmd` on Windows).
        assert!(crate::platform::resolve_shim(&shim_of(&layout, "ay")).is_some());
        // Index pins ay=18 but yanks ay@18 → decide() == Tombstone for the installed ay@17.
        let fake = rollback_index(0, &["ay@18"]);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: Some(17),
        };
        let err = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(
            matches!(err, FlowError::Tombstoned(ref p) if p == "ay"),
            "got {err:?}"
        );
        let shim = shim_of(&layout, "ay");
        assert!(
            std::fs::symlink_metadata(&shim)
                .unwrap()
                .file_type()
                .is_file(),
            "old symlink shim replaced by a tombstone regular file"
        );
        assert!(
            crate::platform::resolve_shim(&shim).is_none(),
            "tombstone no longer forwards anywhere"
        );
        let out = std::process::Command::new(&shim).output().unwrap();
        assert!(!out.status.success(), "tombstone shim exits nonzero");
        assert!(String::from_utf8_lossy(&out.stderr).contains("yanked/revoked"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An install of the build the store already runs fetches nothing and shims nothing.
    ///
    /// The store is SEEDED for real (tree + shims + the readiness marker) rather than
    /// asserted by passing `installed: Some(18)` over an empty prefix: the decision reads
    /// the completeness marker now ([`installed_for_decide`]), and a bare `Some(18)` with
    /// no tree on disk is a state [`crate::ops::active_builds`] — the only production
    /// source of this argument — cannot produce, since it stats a shim's target before
    /// reporting the build.
    #[test]
    fn already_current_is_a_noop() {
        let dir = scratch("current");
        let fake = fixture(&dir);
        let layout = layout(&dir);
        seed_build(&layout, "ay", 18, true);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: Some(18),
        };
        let r = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
        assert!(r.already_current && r.shimmed.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An up-to-date pass re-points a MISSING `current` witness at the build the pin and
    /// the shims already agree on — the `doctor` remedy for a program that is on PATH
    /// with no link selecting it (`NoLiveWitness`). No fetch happens, and a second pass
    /// over the now-healthy store is a no-op.
    #[test]
    fn an_up_to_date_pass_reasserts_a_missing_current_witness() {
        let dir = scratch("witness");
        let fake = fixture(&dir);
        let layout = layout(&dir);
        // Build 18 on disk and shimmed, exactly as an older manager left it: no
        // `store/ay/current`, no channel link.
        let build_dir = layout.build_dir("ay", 18);
        std::fs::create_dir_all(build_dir.join("bin")).unwrap();
        std::fs::write(build_dir.join("bin").join("ay"), b"#!/bin/true\n").unwrap();
        crate::activate::install_shims(&layout, &build_dir, &["ay".to_string()], Aliases::Off)
            .unwrap();
        crate::store::mark_build_ready(&build_dir).unwrap();
        assert!(
            crate::gc::live_builds(&layout).get("ay").is_none(),
            "no witness before the pass"
        );
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: Some(18),
        };
        let r = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
        assert!(r.already_current && r.shimmed.is_empty());
        let live = crate::gc::live_builds(&layout);
        assert_eq!(
            live.get("ay").map(|w| w.build()),
            Some(18),
            "diverged: {:?}",
            live.diverged()
        );
        assert_eq!(
            std::fs::read_link(layout.program_current("ay")).unwrap(),
            build_dir
        );
        assert_eq!(
            std::fs::read_link(layout.channel_current("stable")).unwrap(),
            build_dir
        );
        let r = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
        assert!(r.already_current);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE ROSETTA REPAIR, driven through the real apply decision. A build that is LIVE —
    /// its shims resolve into it, so [`crate::ops::active_builds`] reports it and every
    /// apply path feeds that number to `decide` — but whose `<build>.ready` marker this
    /// slice refuses must be RE-STAGED, not read as already-current.
    ///
    /// Before [`installed_for_decide`] nothing ever re-staged it: `decide` saw only the
    /// shim view, answered UpToDate, `reassert_witness` found a valid witness and returned.
    /// `doctor` FAILed forever ("active ay build 18 store missing/incomplete") and the
    /// structural repair it names — `aterm pkg install ay` — came back through the same
    /// shim view and printed "already current", so the store stayed on the other
    /// architecture's binaries.
    #[test]
    fn a_live_build_this_slice_cannot_vouch_for_is_re_staged() {
        let dir = scratch("otherslice");
        let fake = fixture(&dir);
        let layout = layout(&dir);
        let build_dir = layout.build_dir("ay", 18);
        let fresh = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        // 1. A real install: ay@18 is live, shimmed, and marked by THIS slice.
        let first = install(&fake, &layout, &anchor(), &fresh, fl(0), 0).unwrap();
        assert!(!first.already_current);
        assert!(crate::store::build_is_complete(&build_dir));

        // 2. Forge the marker the x86_64 slice would have left on this machine: the
        //    SIBLING `<build>.ready`, the one file `store::build_is_complete` reads.
        let marker = build_dir.with_file_name("18.ready");
        std::fs::write(&marker, "ok\nplatform=some-other-arch-macos\n").unwrap();
        assert!(
            !crate::store::build_is_complete(&build_dir),
            "PRECONDITION: the tree is not a complete install for this slice"
        );
        assert_eq!(
            crate::ops::active_builds(&layout).get("ay").copied(),
            Some(18),
            "PRECONDITION: the shim view — what every apply path feeds `decide` — says 18"
        );

        // 3. The next native pass, fed exactly what the CLI feeds it.
        let live = InstallRequest {
            installed: Some(18),
            ..fresh
        };
        let again = install(&fake, &layout, &anchor(), &live, fl(0), 0).unwrap();
        assert!(
            !again.already_current,
            "a build this slice cannot vouch for was called up to date — nothing will ever \
             re-stage it, and doctor's `install ay` remedy is a no-op"
        );
        assert_eq!(again.shimmed, vec!["ay".to_string()]);
        assert!(
            crate::store::build_is_complete(&build_dir),
            "the re-stage must leave a marker THIS slice accepts"
        );
        assert!(
            crate::ops::list_installed(&layout)
                .iter()
                .any(|(p, b)| p == "ay" && *b == 18),
            "and the repaired build reads as installed again"
        );
        assert_eq!(
            crate::ops::which(&layout, "ay").unwrap(),
            tool_bin(&build_dir, "ay"),
            "the live tool still resolves into the re-staged tree"
        );

        // 4. The UNMARKED live tree repairs the same way — a marker lost to a restore, a
        //    `mark_build_ready` that failed on a full volume, or the interrupted-swap
        //    recovery that deliberately leaves the tree it recovered unmarked.
        crate::store::clear_build_ready(&build_dir).unwrap();
        let again = install(&fake, &layout, &anchor(), &live, fl(0), 0).unwrap();
        assert!(
            !again.already_current,
            "an unmarked live build must re-stage too"
        );
        assert!(crate::store::build_is_complete(&build_dir));

        // 5. …and a HEALTHY store is still a no-op: the marker is an input to the decision,
        //    never a re-download trigger.
        let again = install(&fake, &layout, &anchor(), &live, fl(0), 0).unwrap();
        assert!(again.already_current && again.shimmed.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreachable_program_is_refused() {
        let dir = scratch("unreach");
        let fake = fixture(&dir);
        let layout = layout(&dir);
        // "dotfiles" is not named in the signed index.
        let req = InstallRequest {
            channel: "stable",
            program: "dotfiles",
            triple: TRIPLE,
            installed: None,
        };
        let err = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(matches!(err, FlowError::NotReachable(..)), "got {err:?}");
        // The refusal answers the follow-up question from the same verified index.
        let rendered = err.to_string();
        assert!(
            rendered.contains("is not named in the signed index"),
            "the grep-stable phrase survives: {rendered}"
        );
        assert!(
            rendered.contains("(it names: "),
            "the verified roster is attached at construction: {rendered}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_root_key_finds_no_index() {
        let dir = scratch("wrongroot");
        let fake = fixture(&dir);
        let layout = layout(&dir);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let err = install(&fake, &layout, &anchor_of(&RELEASE_SEED), &req, fl(0), 0).unwrap_err();
        assert!(matches!(err, FlowError::NoIndex), "got {err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_triple_is_a_clean_skip() {
        let dir = scratch("triple");
        let fake = fixture(&dir);
        let layout = layout(&dir);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: "x86_64-unknown-linux-gnu",
            installed: None,
        };
        let err = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(matches!(err, FlowError::NoArtifact(_)), "got {err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Freshness (§8): a `now` at/after the index's valid_until refuses the install, even
    // though the index is genuinely signed (a stale-index replay defense).
    #[test]
    fn stale_index_is_refused() {
        let dir = scratch("stale");
        let fake = fixture(&dir);
        let layout = layout(&dir);
        // valid_until in the fixture is 2026-07-05T12:00:00Z = 1783252800. Use a later now.
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let err = install(&fake, &layout, &anchor(), &req, fl(0), 2_000_000_000).unwrap_err();
        assert!(matches!(err, FlowError::Stale), "got {err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // §16.4 dispatch / §16.2 gate: an `app-bundle` artifact (the app's in-session apply
    // topology, owned by aterm-gui/aterm-update) is refused by `atpkg install` — driven
    // through the two-anchor app-apply gate, which fails closed because notarization is
    // unproven on the CLI install path, before any download.
    #[test]
    fn refuses_app_bundle_kind() {
        let dir = scratch("appkind");
        let index_body = format!(
            "schema = 2\nindex_build = 41\nvalid_until = \"2099-01-01T00:00:00Z\"\n{}\
             [programs.aterm]\nrepo = \"aterm\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\npin = {{ aterm = 18 }}\n",
            attribution()
        );
        let pkg_body = format!(
            "schema = 2\nprogram = \"aterm\"\nbuild_number = 18\nexposes = []\n\
             [[artifact]]\ntarget = \"{TRIPLE}\"\nkind = \"app-bundle\"\nasset = \"x.dmg\"\nsha256 = \"00\"\n"
        );
        let mut pkg = HashMap::new();
        pkg.insert(
            ("aterm".to_string(), 18u64),
            (
                pkg_body.clone().into_bytes(),
                sign(&RELEASE_SEED, pkg_body.as_bytes()),
            ),
        );
        let fake = Fake {
            index: index_body.clone().into_bytes(),
            index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
            pkg,
            archives: HashMap::new(),
        };
        let req = InstallRequest {
            channel: "stable",
            program: "aterm",
            triple: TRIPLE,
            installed: None,
        };
        let err = install(&fake, &layout(&dir), &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(
            matches!(err, FlowError::AppBundleRefused(ref p) if p == "aterm"),
            "got {err:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Disk preflight is a PURE gate over an injected `available`, so the flow's disk logic is
    // unit-tested WITHOUT a real statvfs (keeping the e2e tests hermetic).
    #[test]
    fn disk_gate_is_pure_and_fails_only_on_a_real_shortfall() {
        // A genuine measured shortfall refuses.
        assert!(matches!(
            disk_gate(u64::MAX, Some(1000)),
            Err(FlowError::InsufficientDisk { .. })
        ));
        // Query failure (None) => fail OPEN.
        assert!(disk_gate(0, None).is_ok());
        // Ample space => ok.
        assert!(disk_gate(1 << 20, Some(u64::MAX)).is_ok());
    }

    #[test]
    fn rfc3339_parses_known_instants() {
        assert_eq!(rfc3339_to_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(rfc3339_to_unix("2026-07-05T12:00:00Z"), Some(1_783_252_800));
        // Malformed → None (the caller treats it as lapsed, fail closed).
        assert_eq!(rfc3339_to_unix("2026-07-05"), None);
        assert_eq!(rfc3339_to_unix("not-a-date"), None);
        assert_eq!(rfc3339_to_unix("2026-13-05T00:00:00Z"), None); // month 13
        // The `Z` suffix is REQUIRED, exactly once, exactly at byte 19: a timezone
        // offset must not be silently read as UTC (up to 14h of fail-open freshness
        // skew), and trailing bytes past the seconds field must not parse either.
        assert_eq!(rfc3339_to_unix("2026-07-05T12:00:00+09:00"), None); // offset, not UTC
        assert_eq!(rfc3339_to_unix("2026-07-05T12:00:00-05:00"), None);
        assert_eq!(rfc3339_to_unix("2026-07-05T12:00:00"), None); // bare, no zone
        assert_eq!(rfc3339_to_unix("2026-07-05T12:00:00GARBAGE"), None);
        assert_eq!(rfc3339_to_unix("2026-07-05T12:00:00Z "), None); // trailing byte
        assert_eq!(rfc3339_to_unix("2026-07-05T12:00:00Zjunk"), None);
        assert_eq!(rfc3339_to_unix("2026-07-05T12:00:00.5Z"), None); // fractional secs
    }

    // --- coherence-group transactional apply (apply_channel over the REAL flow) --------

    /// A single-file USTAR + zstd archive carrying `bin/<program>`.
    fn prog_archive(dir: &Path, program: &str, build: u64) -> PathBuf {
        fn entry(name: &str, content: &[u8]) -> Vec<u8> {
            let mut h = [0u8; 512];
            let nb = name.as_bytes();
            h[..nb.len()].copy_from_slice(nb);
            h[100..108].copy_from_slice(b"0000755\0");
            h[108..116].copy_from_slice(b"0000000\0");
            h[116..124].copy_from_slice(b"0000000\0");
            h[124..136].copy_from_slice(format!("{:011o}\0", content.len()).as_bytes());
            h[136..148].copy_from_slice(b"00000000000\0");
            h[148..156].copy_from_slice(b"        ");
            h[156] = b'0';
            h[257..263].copy_from_slice(b"ustar\0");
            h[263..265].copy_from_slice(b"00");
            let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
            h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
            let mut out = h.to_vec();
            out.extend_from_slice(content);
            out.resize(out.len() + (512 - content.len() % 512) % 512, 0);
            out
        }
        let mut tar = Vec::new();
        tar.extend(entry(
            &format!("bin/{program}"),
            format!("#!/bin/true\n{program}").as_bytes(),
        ));
        tar.resize(tar.len() + 1024, 0);
        let path = dir.join(format!("{program}-{build}.tar.zst"));
        let f = std::fs::File::create(&path).unwrap();
        let mut enc = zstd::Encoder::new(f, 0).unwrap();
        enc.write_all(&tar).unwrap();
        enc.finish().unwrap();
        path
    }

    /// A signed release whose `stable` channel pins a `rustc` coherence group (trust@4821 +
    /// ay@18) plus consistent per-build manifests + archives.
    fn group_fixture(dir: &Path) -> Fake {
        group_fixture_with(dir, &[])
    }

    /// [`group_fixture`] with each program in `unserved` published only for a foreign
    /// triple — how the release looks from a host the publisher does not build it for.
    fn group_fixture_with(dir: &Path, unserved: &[&str]) -> Fake {
        let mut pkg = HashMap::new();
        let mut archives = HashMap::new();
        for (program, build) in [("trust", 4821u64), ("ay", 18u64)] {
            let target = if unserved.contains(&program) {
                "riscv64gc-unknown-linux-gnu"
            } else {
                TRIPLE
            };
            let archive = prog_archive(dir, program, build);
            let sha = crate::tree::file_sha256(&archive).unwrap();
            let probe = dir.join(format!("probe-{program}"));
            crate::extract::extract_tar_zst(&archive, &probe, 10_000_000, 10_000).unwrap();
            let root = crate::tree::tree_root(&probe).unwrap();
            let asset = format!("{program}-{build}.tar.zst");
            let pkg_body = format!(
                "schema = 2\nprogram = \"{program}\"\nversion = \"0.1\"\nbuild_number = {build}\n\
                 exposes = [\"{program}\"]\n\
                 [[artifact]]\ntarget = \"{target}\"\nkind = \"binary\"\nasset = \"{asset}\"\n\
                 sha256 = \"{sha}\"\ntree_root = \"{root}\"\nsize = 100\n\
                 [artifact.cost]\ndisk_installed = 1048576\n"
            );
            pkg.insert(
                (program.to_string(), build),
                (
                    pkg_body.clone().into_bytes(),
                    sign(&RELEASE_SEED, pkg_body.as_bytes()),
                ),
            );
            archives.insert(asset, archive);
        }
        let index_body = format!(
            "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
                          [programs.trust]\nrepo = \"trust\"\ncoherence_group = \"rustc\"\n\
             [programs.ay]\nrepo = \"ay\"\ncoherence_group = \"rustc\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             pin = {{ trust = 4821, ay = 18 }}\n",
            attr = attribution()
        );
        Fake {
            index: index_body.clone().into_bytes(),
            index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
            pkg,
            archives,
        }
    }

    // The rustc tuple moves ATOMICALLY: with ay installed at an old build, apply_channel
    // stages both members then flips both — the whole group ends at its pins.
    #[test]
    fn coherence_group_applies_atomically() {
        let dir = scratch("group-ok");
        let fake = group_fixture(&dir);
        let layout = layout(&dir);
        let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
        let report = apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &installed,
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert_eq!(report.index_build, 41);
        assert_eq!(report.groups.len(), 1, "one coherence group");
        let (group, outcome) = &report.groups[0];
        assert_eq!(group.group.as_deref(), Some("rustc"));
        assert_eq!(
            *outcome,
            TxnOutcome::Applied(vec!["ay".into(), "trust".into()])
        );
        assert_eq!(
            crate::ops::which(&layout, "ay").unwrap(),
            tool_bin(&layout.build_dir("ay", 18), "ay")
        );
        assert_eq!(
            crate::ops::which(&layout, "trust").unwrap(),
            tool_bin(&layout.build_dir("trust", 4821), "trust")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// [`group_fixture`]'s signed release, re-published with the `rustc` tuple pinned ONLY
    /// through a `pin_by_target` overlay for [`TRIPLE`]: the platform-agnostic `pin` names
    /// `trust@4820` (a build with no manifest this fetcher serves, i.e. another target's
    /// pin) and never names `ay`, while the overlay REPLACES trust with 4821 and ADDS
    /// ay@18. A lane that reads the raw pin plans `trust` alone and stages a build that
    /// cannot be fetched; only the per-target view reaches the builds this host can run.
    fn overlay_group_fixture(dir: &Path) -> Fake {
        let mut fake = group_fixture(dir);
        let index_body = format!(
            "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
             [programs.trust]\nrepo = \"trust\"\ncoherence_group = \"rustc\"\n\
             [programs.ay]\nrepo = \"ay\"\ncoherence_group = \"rustc\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             pin = {{ trust = 4820 }}\n\
             pin_by_target = {{ \"{TRIPLE}\" = {{ trust = 4821, ay = 18 }} }}\n",
            attr = attribution()
        );
        fake.index_sig = sign(&RELEASE_SEED, index_body.as_bytes());
        fake.index = index_body.into_bytes();
        fake
    }

    /// The update lane honours the per-target pin: `apply_channel` and `apply_program`
    /// plan the tuple from `channel_for(.., TRIPLE)`, so the overlay-ADDED `ay` is a
    /// member at all and `trust` lands on the overlay's 4821. Before, both read the raw
    /// channel: `ay` was unpinned, the group was `trust` alone with nothing installed, and
    /// the update skipped it (or `apply_program` answered `NotPinned`).
    #[test]
    fn update_lanes_apply_the_per_target_pin_overlay() {
        for lane in ["channel", "program"] {
            let dir = scratch(&format!("overlay-update-{lane}"));
            let fake = overlay_group_fixture(&dir);
            let layout = layout(&dir);
            let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
            let report = if lane == "channel" {
                apply_channel(
                    &fake,
                    &layout,
                    &anchor(),
                    "stable",
                    TRIPLE,
                    &installed,
                    &[],
                    fl(0),
                    0,
                )
            } else {
                apply_program(
                    &fake,
                    &layout,
                    &anchor(),
                    "stable",
                    TRIPLE,
                    "ay",
                    &installed,
                    &[],
                    fl(0),
                    0,
                )
            }
            .unwrap_or_else(|e| panic!("{lane}: the overlaid tuple is planned: {e}"));
            assert_eq!(report.groups.len(), 1, "{lane}: one coherence group");
            assert_eq!(
                report.groups[0].1,
                TxnOutcome::Applied(vec!["ay".into(), "trust".into()]),
                "{lane}: the whole overlaid tuple moves"
            );
            assert_eq!(
                crate::ops::which(&layout, "trust").unwrap(),
                tool_bin(&layout.build_dir("trust", 4821), "trust"),
                "{lane}: trust lands on the overlay's build, not the raw pin"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// The tuple-bootstrap lane stages what its caller planned: `bootstrap_group` resolves
    /// the same per-target view the CLI's plan and `group_missing_triple` prescan used, so
    /// a group the prescan says is served installs whole. Before, it re-read the raw pin
    /// and aborted every pass at `trust` (pkg-trust-4820 is not served for this host).
    #[test]
    fn bootstrap_group_stages_the_per_target_pin_overlay() {
        let dir = scratch("overlay-bootstrap");
        let fake = overlay_group_fixture(&dir);
        let layout = layout(&dir);
        let index = resolve_verified_index(&fake, &layout, &anchor(), fl(0), 0).unwrap();
        let ch = index.channel_for("stable", TRIPLE).unwrap();
        let group = plan_groups(&index, &ch)
            .into_iter()
            .find(|g| g.group.as_deref() == Some("rustc"))
            .expect("the overlay pins the rustc tuple");
        assert_eq!(
            group_missing_triple(&fake, &index, "stable", TRIPLE, &group.members),
            None,
            "the prescan sees the tuple served on this target"
        );
        let mut resolved = BTreeMap::new();
        let (outcome, _) = bootstrap_group(
            &fake,
            &layout,
            &index,
            "stable",
            TRIPLE,
            &group,
            &BTreeMap::new(),
            &mut resolved,
        )
        .unwrap();
        assert_eq!(
            outcome,
            TxnOutcome::Applied(vec!["ay".into(), "trust".into()]),
            "the bootstrap stages the overlay's builds, not the raw pin"
        );
        assert_eq!(
            crate::ops::which(&layout, "trust").unwrap(),
            tool_bin(&layout.build_dir("trust", 4821), "trust")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `plan_update` decides against the per-target pin: trust@4821 installed from the
    /// overlay is up to date (not a "move" to another target's 4820), and the overlay-added
    /// `ay` is pinned (not `NotPinned`).
    #[test]
    fn plan_update_decides_against_the_per_target_pin_overlay() {
        let dir = scratch("overlay-plan");
        let fake = overlay_group_fixture(&dir);
        let layout = layout(&dir);
        let trust = plan_update(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            "trust",
            Some(4821),
            fl(0),
            0,
        )
        .unwrap();
        assert_eq!(trust.decision, ApplyDecision::UpToDate);
        assert!(trust.current_build_ok);
        let ay = plan_update(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            "ay",
            Some(17),
            fl(0),
            0,
        )
        .unwrap();
        assert_eq!(ay.decision, ApplyDecision::Install);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Atomicity under failure: corrupt one member's archive so its stage fails; the WHOLE
    // group aborts during the stage phase, NOTHING is flipped, AND the already-staged
    // sibling's build is DISCARDED (not left complete-but-inactive) — then a retry with the
    // corruption cleared heals the tuple coherently.
    #[test]
    fn a_member_stage_failure_aborts_the_group_and_a_retry_heals_it() {
        let dir = scratch("group-abort");
        let fake = group_fixture(&dir);
        // Corrupt trust's archive → its sha256 re-verify fails at stage.
        let trust_archive = fake.archives.get("trust-4821.tar.zst").unwrap().clone();
        std::fs::write(&trust_archive, b"corrupt-not-the-signed-bytes").unwrap();
        let layout = layout(&dir);
        let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);

        let report = apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &installed,
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert!(
            matches!(
                report.groups[0].1,
                TxnOutcome::Aborted {
                    during_flip: false,
                    ..
                }
            ),
            "a stage failure aborts BEFORE any flip: {:?}",
            report.groups[0].1
        );
        // Nothing flipped.
        assert!(
            crate::ops::which(&layout, "trust").is_none(),
            "trust never flipped"
        );
        assert!(
            crate::ops::which(&layout, "ay").is_none(),
            "ay never flipped (group is atomic)"
        );
        // CRUCIAL — the already-staged ay@18 build was DISCARDED, so it can't be mis-read as
        // active on the next run (which would permanently split the tuple).
        assert!(
            !layout.build_dir("ay", 18).exists(),
            "aborted-group staged build is discarded"
        );
        assert!(
            !crate::store::build_is_complete(&layout.build_dir("ay", 18)),
            "no lingering completeness marker for the discarded build"
        );

        // Heal: clear the corruption (re-create the real signed archive) and retry.
        prog_archive(&dir, "trust", 4821);
        let report2 = apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &installed,
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert_eq!(
            report2.groups[0].1,
            TxnOutcome::Applied(vec!["ay".into(), "trust".into()])
        );
        assert_eq!(
            crate::ops::which(&layout, "ay").unwrap(),
            tool_bin(&layout.build_dir("ay", 18), "ay")
        );
        assert_eq!(
            crate::ops::which(&layout, "trust").unwrap(),
            tool_bin(&layout.build_dir("trust", 4821), "trust")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The abort discard must never delete a build that was ALREADY LIVE when this
    // transaction re-staged it. The shim-derived `installed` view goes silent when a
    // program's tools are gone (`atpkg unlink`, tombstones, dev-link mode) while the
    // `current` authority links still name the live build, so `decide` legitimately
    // returns Install for the build that is already active. A SIBLING member then failing
    // to stage must not take that live toolchain down with it.
    #[test]
    fn a_group_abort_never_discards_a_member_that_was_already_live() {
        let dir = scratch("group-abort-live");
        let fake = group_fixture(&dir);
        let layout = layout(&dir);

        // 1. Really install the tuple: ay@18 + trust@4821 go live.
        let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
        apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &installed,
            &[],
            fl(0),
            0,
        )
        .unwrap();
        let ay18 = layout.build_dir("ay", 18);
        assert!(
            crate::store::build_is_complete(&ay18),
            "PRECONDITION: ay@18 is installed and complete"
        );

        // 2. `atpkg unlink`-shaped state: ay's shims are gone — the primary AND the
        //    `alab-ay` alias the flip laid beside it (ay is ALab's own in this index), since
        //    the alias resolves into the same build and would keep the shim view talking —
        //    so the SHIM view is silent for ay while the authority link still names ay@18
        //    as live.
        std::fs::remove_file(layout.shim(&tool("ay"))).unwrap();
        std::fs::remove_file(layout.shim(&tool("alab-ay"))).unwrap();
        assert!(
            !crate::ops::active_builds(&layout).contains_key("ay"),
            "PRECONDITION: the shim view no longer knows ay — `decide` will re-Install 18"
        );
        assert_eq!(
            std::fs::read_link(layout.program_current("ay")).unwrap(),
            ay18,
            "PRECONDITION: the authority link still names ay@18, so it IS live"
        );

        // 3. A SIBLING member fails to stage → the group aborts after ay re-staged fine.
        std::fs::write(fake.archives.get("trust-4821.tar.zst").unwrap(), b"corrupt").unwrap();
        let report = apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &std::collections::BTreeMap::from([("trust".to_string(), 4820u64)]),
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert!(
            matches!(
                report.groups[0].1,
                TxnOutcome::Aborted {
                    during_flip: false,
                    ..
                }
            ),
            "PRECONDITION: the group aborted in the stage phase: {:?}",
            report.groups[0].1
        );

        // The casualty test: the failing member is `trust`; `ay` must survive intact.
        assert!(
            ay18.is_dir(),
            "the abort discarded a build that was already LIVE — the toolchain is gone"
        );
        assert!(
            crate::store::build_is_complete(&ay18),
            "the abort took down the completeness marker of a LIVE build"
        );
        assert_eq!(
            std::fs::read_link(layout.program_current("ay")).unwrap(),
            ay18,
            "the authority link must still resolve"
        );
        assert!(
            crate::ops::list_installed(&layout)
                .iter()
                .any(|(p, b)| p == "ay" && *b == 18),
            "ay@18 must still read as installed after a sibling's failure"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same repair inside the GROUP transaction, which decides per member from its own
    /// `decide` call: a tuple member whose store tree this slice cannot vouch for must be
    /// re-staged, not held at UpToDate beside its healthy sibling. `update` routes every
    /// coherence-group member (trust, ay, …) through here, so without this the Rosetta
    /// store and the marker-less live tree were permanent for exactly the programs the
    /// platform record was written for.
    #[test]
    fn a_group_member_this_slice_cannot_vouch_for_is_re_staged() {
        let dir = scratch("group-otherslice");
        let fake = group_fixture(&dir);
        let layout = layout(&dir);

        // 1. Really install the tuple: ay@18 + trust@4821 go live.
        apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &std::collections::BTreeMap::from([("ay".to_string(), 17u64)]),
            &[],
            fl(0),
            0,
        )
        .unwrap();
        let ay18 = layout.build_dir("ay", 18);
        assert!(
            crate::store::build_is_complete(&ay18),
            "PRECONDITION: ay@18 is installed and complete"
        );

        // 2. ay@18 loses the marker this slice reads — the other slice's `platform=` record,
        //    or a restore/interrupted-swap recovery that left the tree unmarked. The shims
        //    still resolve into it, so the shim view keeps reporting 18.
        crate::store::clear_build_ready(&ay18).unwrap();
        let live = crate::ops::active_builds(&layout);
        assert_eq!(
            live.get("ay").copied(),
            Some(18),
            "PRECONDITION: the shim view the update lane feeds `decide` still says 18"
        );
        assert_eq!(live.get("trust").copied(), Some(4821));

        // 3. The next pass, fed that map exactly as `cmd_update_all` feeds it.
        let report = apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &live,
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert_eq!(report.groups.len(), 1, "one coherence group");
        assert_eq!(
            report.groups[0].1,
            TxnOutcome::Applied(vec!["ay".into()]),
            "the tuple must re-stage the member this slice cannot vouch for (its healthy \
             sibling stays put)"
        );
        assert!(
            crate::store::build_is_complete(&ay18),
            "the re-stage leaves a marker THIS slice accepts"
        );
        assert_eq!(
            crate::ops::which(&layout, "ay").unwrap(),
            tool_bin(&ay18, "ay"),
            "and the tool still resolves into the re-staged tree"
        );

        // A healthy tuple is still a no-op — the completeness input never re-downloads a
        // store that is sound.
        let report = apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &crate::ops::active_builds(&layout),
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert_eq!(report.groups[0].1, TxnOutcome::UpToDate);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The missing-triple hold at the flow layer: an installed tuple (ay@17 + trust@4820)
    // whose new pin trust@4821 publishes no artifact for this triple is held whole on its
    // current builds — never staged, never aborted, nothing resolved for download.
    #[test]
    fn an_installed_group_whose_new_pin_lacks_this_triple_is_held_whole() {
        let dir = scratch("group-unpublished");
        let fake = group_fixture_with(&dir, &["trust"]);
        let layout = layout(&dir);
        seed_build(&layout, "ay", 17, true);
        seed_build(&layout, "trust", 4820, true);
        let installed = std::collections::BTreeMap::from([
            ("ay".to_string(), 17u64),
            ("trust".to_string(), 4820u64),
        ]);
        let report = apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &installed,
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert_eq!(
            report.groups[0].1,
            TxnOutcome::Unpublished {
                member: "trust".into(),
                build: 4821,
                triple: TRIPLE.into(),
            },
            "an unpublished pin holds the tuple, it does not abort it"
        );
        assert_eq!(
            crate::ops::which(&layout, "ay").unwrap(),
            tool_bin(&layout.build_dir("ay", 17), "ay")
        );
        assert_eq!(
            crate::ops::which(&layout, "trust").unwrap(),
            tool_bin(&layout.build_dir("trust", 4820), "trust")
        );
        for (p, b) in [("ay", 18u64), ("trust", 4821u64)] {
            assert!(!layout.build_dir(p, b).exists(), "{p}@{b} was staged");
            assert!(!layout.staging_dir(p).exists(), "{p} touched staging/");
        }
        assert!(
            report.resolved_assets.is_empty() && report.applied.is_empty(),
            "nothing resolved, nothing applied: {:?} {:?}",
            report.resolved_assets,
            report.applied
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A [`Fake`] that logs every per-build manifest it is asked for.
    struct LoggedFake {
        inner: Fake,
        manifests: std::cell::RefCell<Vec<(String, u64)>>,
    }
    impl Fetcher for LoggedFake {
        fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
            self.inner.index_candidates()
        }
        fn pkg_manifest(
            &self,
            repo: &str,
            program: &str,
            build: u64,
        ) -> Result<(Vec<u8>, Vec<u8>), String> {
            self.manifests
                .borrow_mut()
                .push((program.to_string(), build));
            self.inner.pkg_manifest(repo, program, build)
        }
        fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
            self.inner.download(repo, asset, dest)
        }
    }

    // The hold reads only the members the pass would move. ay is up to date on build 18,
    // whose signed manifest has no row for this triple; trust moves 4820 → 4821, which is
    // published here. Nothing of ay's is staged, so its manifest is never even fetched.
    #[test]
    fn an_up_to_date_member_whose_pin_lacks_this_triple_never_holds_its_sibling() {
        let dir = scratch("group-uptodate-unserved");
        let fake = LoggedFake {
            inner: group_fixture_with(&dir, &["ay"]),
            manifests: std::cell::RefCell::default(),
        };
        let layout = layout(&dir);
        seed_build(&layout, "ay", 18, true);
        seed_build(&layout, "trust", 4820, true);
        let installed = std::collections::BTreeMap::from([
            ("ay".to_string(), 18u64),
            ("trust".to_string(), 4820u64),
        ]);
        let report = apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &installed,
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert_eq!(
            report.groups[0].1,
            TxnOutcome::Applied(vec!["trust".into()]),
            "an up-to-date member's manifest never holds the sibling that moves"
        );
        assert_eq!(
            crate::ops::which(&layout, "trust").unwrap(),
            tool_bin(&layout.build_dir("trust", 4821), "trust")
        );
        assert_eq!(
            crate::ops::which(&layout, "ay").unwrap(),
            tool_bin(&layout.build_dir("ay", 18), "ay")
        );
        // One manifest request per member the pass stages — the walk's, then the stage's
        // own — exactly the pair the disk preflight and the stage always made; the
        // up-to-date member's is never among them.
        assert_eq!(
            *fake.manifests.borrow(),
            [
                ("trust".to_string(), 4821u64),
                ("trust".to_string(), 4821u64)
            ],
            "the walk costs no request the old path did not make"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // A recalled member whose own new pin is the unpublished one is told that its
    // replacement is missing, and which build that is. (A recalled member whose sibling's
    // pin is missing is named with that sibling's build — the cli revoked-build test.)
    #[test]
    fn a_recalled_member_whose_own_pin_is_unpublished_is_told_its_replacement_build() {
        assert_eq!(
            recalled_unpublished_notice("tb", "tb", 6, TRIPLE),
            format!(
                "atpkg: tb was recalled and its replacement (tb build 6) is not published for \
                 {TRIPLE} — its commands are disabled until it is."
            )
        );
    }

    // --- rollback + local pin + apply_program (steps 9/10/11) --------------------------

    /// Lay down a COMPLETE build dir with `bin/<program>`; `activate` also shims + activates
    /// it (making it the ACTIVE build).
    fn seed_build(layout: &Layout, program: &str, build: u64, activate: bool) {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(
            dir.join("bin").join(tool(program).exe_file()),
            b"#!/bin/true\n",
        )
        .unwrap();
        if activate {
            crate::activate::install_shims(
                layout,
                &dir,
                &[program.to_string()],
                crate::activate::Aliases::Off,
            )
            .unwrap();
            activate_channel(layout, "stable", &dir).unwrap();
        }
        crate::store::mark_build_ready(&dir).unwrap();
    }

    /// An index-only Fake (empty pkg/archives — rollback never downloads) pinning `ay` with a
    /// configurable `min_build` + `yanked` list, so the floor/yank gate is exercised.
    fn rollback_index(min_build: u64, yanked: &[&str]) -> Fake {
        let yanked_toml = if yanked.is_empty() {
            String::new()
        } else {
            let items: Vec<String> = yanked.iter().map(|y| format!("\"{y}\"")).collect();
            format!("yanked = [{}]\n", items.join(", "))
        };
        let index_body = format!(
            "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
                          [programs.ay]\nrepo = \"ay\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = {min_build}\n\
             {yanked_toml}pin = {{ ay = 18 }}\n",
            attr = attribution()
        );
        Fake {
            index: index_body.clone().into_bytes(),
            index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
            pkg: HashMap::new(),
            archives: HashMap::new(),
        }
    }

    /// [`rollback_index`] with the floor published PER PROGRAM (`min_build_by_program`), the
    /// channel-wide floor left at 0 — the only shape that can floor one program of a channel
    /// whose members have independent build counters without refusing the others' builds.
    fn rollback_index_per_program(floor_ay: u64) -> Fake {
        let index_body = format!(
            "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
                          [programs.ay]\nrepo = \"ay\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             min_build_by_program = {{ ay = {floor_ay} }}\npin = {{ ay = 18 }}\n",
            attr = attribution()
        );
        Fake {
            index: index_body.clone().into_bytes(),
            index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
            pkg: HashMap::new(),
            archives: HashMap::new(),
        }
    }

    /// [`group_fixture`] but whose stable channel YANKS `ay@18`, so `decide` tombstones the
    /// group (to prove a pin never suppresses a tombstone).
    /// EXHAUSTIVE CONFORMANCE FOR THE REMOVED-MEMBER HOLD.
    ///
    /// This one predicate — what an unattended update does to a coherence tuple when
    /// the user has deliberately uninstalled part of it — took a defect in three
    /// consecutive audit rounds. Every fix was reasonable and every one was wrong in a
    /// NEW way: reinstalling the deleted member, tombstoning a member that had a valid
    /// upgrade waiting, dropping a removed-but-installed member so its revoked build
    /// stayed runnable while its siblings moved, and reporting "up to date" over an
    /// empty tuple. Inspection kept missing them because the state space is small but
    /// not small enough to hold in your head: per member, {absent, safe, revoked} ×
    /// {recorded-removed or not}, against {clean, installed-build-yanked, pin-yanked}.
    ///
    /// So it is enumerated instead of argued about. The assertions below are the
    /// INVARIANTS, not the expected outputs of any particular branch — they are what
    /// must hold whatever the implementation decides:
    ///
    ///   I1. A member that is recorded-removed AND absent is never installed. That is
    ///       the whole point of the record.
    ///   I2. A revoked installed build is never left RUNNABLE. It is either upgraded to
    ///       the valid pin or its tools are tombstoned — never silently kept.
    ///   I3. The pass never claims `UpToDate` while some installed member is revoked.
    #[test]
    fn the_removed_member_hold_is_exhaustively_safe() {
        #[derive(Clone, Copy, Debug)]
        enum Ay {
            Absent,
            Safe,
            Revoked,
        }
        type Fixture = fn(&Path) -> Fake;
        let channels: [(&str, Fixture); 3] = [
            ("clean", group_fixture),
            ("yank-installed", group_fixture_yanking_ay17),
            ("yank-pin", group_fixture_yanking_ay18),
        ];
        let mut checked = 0;
        for (cname, make) in channels {
            for ay in [Ay::Absent, Ay::Safe, Ay::Revoked] {
                for trust_installed in [false, true] {
                    for removed in [&[][..], &["ay"][..], &["trust"][..], &["ay", "trust"][..]] {
                        // `[packages].exclude` rides the same deliberately-absent
                        // predicate as the removal record, so it gets the same
                        // exhaustive treatment — including the case where both
                        // name the same member, and where an exclusion names a
                        // PRESENT member (which must change nothing: exclude is
                        // "do not pull in", never "drop what is here").
                        for excluded in [&[][..], &["ay"][..], &["trust"][..]] {
                            let excluded: Vec<String> =
                                excluded.iter().map(|s| s.to_string()).collect();
                            let label = format!(
                                "{cname}-{ay:?}-t{trust_installed}-r{}-x{}",
                                removed.len(),
                                excluded.len()
                            );
                            let dir = scratch(&format!("hold-{label}"));
                            let fake = make(&dir);
                            let layout = layout(&dir);
                            let mut installed = std::collections::BTreeMap::new();
                            match ay {
                                Ay::Absent => {}
                                Ay::Safe => {
                                    seed_build(&layout, "ay", 18, true);
                                    installed.insert("ay".to_string(), 18u64);
                                }
                                Ay::Revoked => {
                                    seed_build(&layout, "ay", 17, true);
                                    installed.insert("ay".to_string(), 17u64);
                                }
                            }
                            if trust_installed {
                                seed_build(&layout, "trust", 4821, true);
                                installed.insert("trust".to_string(), 4821u64);
                            }
                            if !removed.is_empty() {
                                // The prefix exists only once something has been seeded, and
                                // the absent/absent cases seed nothing.
                                std::fs::create_dir_all(&layout.prefix).unwrap();
                                std::fs::write(layout.removed(), removed.join("\n")).unwrap();
                            }

                            let report = apply_channel(
                                &fake,
                                &layout,
                                &anchor(),
                                "stable",
                                TRIPLE,
                                &installed,
                                &excluded,
                                fl(0),
                                0,
                            );
                            let Ok(report) = report else {
                                // A resolve/verify failure is not this predicate's business.
                                continue;
                            };
                            checked += 1;
                            let after = crate::ops::active_builds(&layout);

                            // I1: a recorded-removed, ABSENT member is never installed.
                            for program in removed {
                                let was_absent = !installed.contains_key(*program);
                                if was_absent {
                                    assert!(
                                        !after.contains_key(*program),
                                        "{label}: {program} was deleted on purpose and came back"
                                    );
                                }
                            }

                            // I1x: an EXCLUDED, absent member is never pulled in — the
                            // promise `uninstall` makes when it names `[packages].exclude`
                            // as the way to drop one program and stay adopted. Before the
                            // exclude wire existed, the next unattended tick reinstalled
                            // the excluded sibling (multi-GB, unannounced) via the
                            // coherence pull-in.
                            for program in &excluded {
                                if !installed.contains_key(program) {
                                    assert!(
                                        !after.contains_key(program),
                                        "{label}: excluded {program} was pulled back in"
                                    );
                                }
                            }

                            // I1x-present: an exclusion naming a PRESENT member is not
                            // license to drop or freeze it — asserted where the channel
                            // gives survival a right answer. In `clean` nothing
                            // legitimately kills a present member; in `yank-installed`
                            // the excluded-but-present member must still UPGRADE to the
                            // valid pin (exclusion never freezes). `yank-pin` proves
                            // nothing here: it tombstones ay@18 with or without the
                            // exclusion, and this invariant's first draft asserting
                            // survival there was refuted by its own enumeration.
                            if cname == "clean" {
                                for program in &excluded {
                                    if installed.contains_key(program) {
                                        assert!(
                                            after.contains_key(program),
                                            "{label}: excluding present {program} dropped it"
                                        );
                                    }
                                }
                            }
                            if cname == "yank-installed"
                                && matches!(ay, Ay::Revoked)
                                && excluded.iter().any(|p| p == "ay")
                            {
                                assert_eq!(
                                    after.get("ay").copied(),
                                    Some(18),
                                    "{label}: excluding present ay froze its upgrade"
                                );
                            }

                            // I2: a revoked installed build is never left runnable.
                            if matches!(ay, Ay::Revoked) && cname != "clean" {
                                let live = after.get("ay").copied();
                                let runnable = crate::ops::which(&layout, "ay").is_some();
                                let safe = live.is_some_and(|b| b == 18);
                                assert!(
                                    safe || !runnable,
                                    "{label}: revoked ay is still runnable at {live:?}"
                                );
                            }

                            // I4: A MEMBER WITH A VALID REPLACEMENT IS NEVER LEFT DEAD.
                            // I2 alone calls tombstoning "safe", and it is — but it is the
                            // WRONG safe answer when the channel is offering a fix, and
                            // that is exactly the shape that shipped: a routine yank
                            // disabled a program that had a working upgrade waiting, and
                            // the corpse then vanished from `active_builds` so no later
                            // pass could see or repair it. An invariant set that only asks
                            // "is anything unsafe running" cannot see this; it has to ask
                            // "did anything that should live, die".
                            if cname == "yank-installed"
                                && matches!(ay, Ay::Revoked)
                                && !(removed.contains(&"ay") && !installed.contains_key("ay"))
                            {
                                assert!(
                                    crate::ops::which(&layout, "ay").is_some(),
                                    "{label}: ay had a valid pin (18) and was killed instead \
                                 of upgraded"
                                );
                            }

                            // I5: THE ORDINARY TICK STILL MOVES. Five independent
                            // derivations all flagged what my own four invariants never
                            // said: the case this predicate meets on almost every run is
                            // "nothing is revoked", and freezing the members that ARE here
                            // — silently, forever — is the failure that costs a lab machine
                            // its updates. A publisher usually ships a fix as a NEW PIN
                            // without yanking the old build, so a frozen group never
                            // receives it (2026-08-20 independent derivation).
                            if cname == "clean" && matches!(ay, Ay::Safe) {
                                // ay@18 is already the pin, so the assertion that matters is
                                // that the pass did not DROP it while excluding a sibling.
                                assert_eq!(
                                    after.get("ay").copied(),
                                    Some(18),
                                    "{label}: a present member was lost while holding for an \
                                 absent one"
                                );
                            }

                            // I6: THE HOLD ONLY FIRES ON A REAL ABSENCE. A stale record for
                            // a member that a signed `requires` pull-in has since
                            // reinstalled must not hold anything: it is present, and the
                            // promise the record encodes ("do not put it back") is already
                            // kept.
                            let stale_only = !removed.is_empty()
                                && removed.iter().all(|m| installed.contains_key(*m));
                            if stale_only && cname == "yank-installed" && matches!(ay, Ay::Revoked)
                            {
                                assert!(
                                    !crate::ops::which(&layout, "ay").is_some()
                                        || after.get("ay").copied() == Some(18),
                                    "{label}: a stale removal record froze a healthy tuple"
                                );
                            }

                            // I3: never "up to date" over a revocation.
                            if matches!(ay, Ay::Revoked) && cname != "clean" {
                                for (_, outcome) in &report.groups {
                                    assert!(
                                        !matches!(outcome, TxnOutcome::UpToDate),
                                        "{label}: reported UpToDate over a revoked build"
                                    );
                                }
                            }
                            let _ = std::fs::remove_dir_all(&dir);
                        }
                    }
                }
            }
        }
        assert!(checked >= 72, "the enumeration ran only {checked} cases");
    }

    fn group_fixture_yanking_ay18(dir: &Path) -> Fake {
        let mut f = group_fixture(dir);
        let index_body = format!(
            "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
                          [programs.trust]\nrepo = \"trust\"\ncoherence_group = \"rustc\"\n\
             [programs.ay]\nrepo = \"ay\"\ncoherence_group = \"rustc\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             yanked = [\"ay@18\"]\n\
             pin = {{ trust = 4821, ay = 18 }}\n",
            attr = attribution()
        );
        f.index = index_body.clone().into_bytes();
        f.index_sig = sign(&RELEASE_SEED, index_body.as_bytes());
        f
    }

    // THE pin-authority test: a local pin freezes the WHOLE group on its current builds; the
    // upgrade is suppressed, nothing is staged, and the sibling is not pulled in.
    #[test]
    fn a_local_pin_holds_a_whole_group_and_never_moves_it() {
        let dir = scratch("pin-holds");
        let fake = group_fixture(&dir);
        let layout = layout(&dir);
        seed_build(&layout, "ay", 17, true); // ay@17 active
        crate::pin::set_pinned(&layout, "ay", true).unwrap();
        let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
        let report = apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &installed,
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].1, TxnOutcome::Pinned(vec!["ay".into()]));
        assert_eq!(
            crate::ops::active_builds(&layout).get("ay").copied(),
            Some(17),
            "ay held at 17"
        );
        assert!(!layout.build_dir("ay", 18).exists(), "no upgrade staged");
        assert!(
            !layout.build_dir("trust", 4821).exists(),
            "sibling not pulled in"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fixture: the pin (ay=18) is VALID, but the currently-installed build ay@17 is YANKED.
    /// decide() returns Install (force-upgrade off 17), NOT Tombstone.
    fn group_fixture_yanking_ay17(dir: &Path) -> Fake {
        let mut f = group_fixture(dir);
        let index_body = format!(
            "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
                          [programs.trust]\nrepo = \"trust\"\ncoherence_group = \"rustc\"\n\
             [programs.ay]\nrepo = \"ay\"\ncoherence_group = \"rustc\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             yanked = [\"ay@17\"]\n\
             pin = {{ trust = 4821, ay = 18 }}\n",
            attr = attribution()
        );
        f.index = index_body.clone().into_bytes();
        f.index_sig = sign(&RELEASE_SEED, index_body.as_bytes());
        f
    }

    // THE critical-fix regression: a local pin must NOT keep a YANKED current build running.
    // ay@17 is active + pinned, but 17 is yanked and the pin (18) is valid — the pin is IGNORED
    // and the tuple force-upgrades off the revoked build, rather than freezing on it.
    #[test]
    fn a_local_pin_does_not_hold_a_yanked_current_build() {
        let dir = scratch("pin-yanked-current");
        let fake = group_fixture_yanking_ay17(&dir);
        let layout = layout(&dir);
        seed_build(&layout, "ay", 17, true); // ay@17 active (now yanked)
        crate::pin::set_pinned(&layout, "ay", true).unwrap();
        let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
        let report = apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &installed,
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert!(
            !matches!(report.groups[0].1, TxnOutcome::Pinned(_)),
            "pin must be ignored when the current build is yanked: {:?}",
            report.groups[0].1
        );
        assert_eq!(
            crate::ops::active_builds(&layout).get("ay").copied(),
            Some(18),
            "force-upgraded off the yanked 17 to the valid pin 18"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // A pin NEVER suppresses a tombstone: a yanked pin tombstones the group even when pinned.
    #[test]
    fn a_pin_never_suppresses_a_tombstone() {
        let dir = scratch("pin-tombstone");
        let fake = group_fixture_yanking_ay18(&dir);
        let layout = layout(&dir);
        seed_build(&layout, "ay", 17, true);
        crate::pin::set_pinned(&layout, "ay", true).unwrap();
        let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
        let report = apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &installed,
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert!(
            matches!(report.groups[0].1, TxnOutcome::Tombstoned(_)),
            "pin is ignored when the gate tombstones: {:?}",
            report.groups[0].1
        );
        // §7 (step 21): the tombstoned member's OLD working shim is not merely reported — it is
        // actively DISABLED. bin/ay flips from the live symlink into ay@17 to an executable
        // failing tombstone script that exits nonzero.
        let shim = shim_of(&layout, "ay");
        let meta = std::fs::symlink_metadata(&shim).unwrap();
        assert!(
            meta.file_type().is_file(),
            "shim replaced by a tombstone regular file, not a symlink"
        );
        assert!(
            crate::platform::resolve_shim(&shim).is_none(),
            "tombstone no longer forwards anywhere"
        );
        let out = std::process::Command::new(&shim).output().unwrap();
        assert!(!out.status.success(), "tombstone shim exits nonzero");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("yanked/revoked"),
            "tombstone shim explains itself on stderr"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// [`fixture`] — the SINGLETON `ay` (no `coherence_group`) — with its own pin YANKED,
    /// so `decide` tombstones it and the pass disables its live shims for real.
    fn fixture_yanking_the_pin(dir: &Path) -> Fake {
        let mut f = fixture(dir);
        let index_body = format!(
            "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
             [programs.ay]\nrepo = \"ay\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             yanked = [\"ay@18\"]\n\
             pin = {{ ay = 18 }}\n",
            attr = attribution()
        );
        f.index = index_body.clone().into_bytes();
        f.index_sig = sign(&RELEASE_SEED, index_body.as_bytes());
        f
    }

    // THE TOMBSTONE'S OWN INSTRUCTION HAS TO WORK. The failing shim says "run `aterm pkg
    // update`", and for a tombstoned SINGLETON (or a tuple every member of which was
    // tombstoned) the plain pass used to do NOTHING: `installed` is the shim view, a
    // tombstone resolves nowhere, so the group had no installed member and `apply_group`
    // skipped it — not re-staged, not even reported, while the row kept reading
    // `tombstoned` and the commands kept failing. Only `aterm pkg update <name>` recovered
    // it, and on a machine that installed one program by name (never adopted the set, no
    // `auto_install`) the set-completion lane cannot stand in.
    #[test]
    fn a_plain_update_revives_a_tombstoned_singleton_when_its_pin_is_good_again() {
        let dir = scratch("tombstone-singleton-revive");
        let layout = layout(&dir);
        // 1. ay@18 is live, and the publisher yanks the pin: the pass tombstones it.
        seed_build(&layout, "ay", 18, true);
        let installed = std::collections::BTreeMap::from([("ay".to_string(), 18u64)]);
        let report = apply_channel(
            &fixture_yanking_the_pin(&dir),
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &installed,
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert!(
            matches!(report.groups[0].1, TxnOutcome::Tombstoned(_)),
            "the yanked pin tombstones the singleton: {:?}",
            report.groups[0].1
        );
        let shim = shim_of(&layout, "ay");
        assert!(
            crate::platform::resolve_shim(&shim).is_none(),
            "bin/ay is now the failing tombstone that says to run `aterm pkg update`"
        );
        // PRECONDITION: the shim view `cmd_update_all_code` feeds this lane is SILENT for
        // ay — which is the whole reason the group used to be invisible to it.
        let live = crate::ops::active_builds(&layout);
        assert!(
            !live.contains_key("ay"),
            "a tombstone resolves nowhere, so the shim view does not name ay"
        );
        // 2. The publisher re-points the pin at a good build. A PLAIN `update` pass — fed
        //    that silent map, exactly as the CLI feeds it — must re-stage the singleton.
        let report = apply_channel(
            &fixture(&dir),
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &live,
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert_eq!(
            report.groups.len(),
            1,
            "the tombstoned singleton is still a group this machine HAS: {:?}",
            report.groups
        );
        assert_eq!(
            report.groups[0].1,
            TxnOutcome::Applied(vec!["ay".into()]),
            "decide sees None (the silence IS the self-heal) and returns Install: {:?}",
            report.groups[0].1
        );
        assert_eq!(
            crate::ops::which(&layout, "ay").unwrap(),
            tool_bin(&layout.build_dir("ay", 18), "ay"),
            "the tombstone is replaced by a working shim — ay runs again"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rollback_selects_highest_gate_valid_build_below_current() {
        let dir = scratch("rollback-basic");
        let fake = rollback_index(0, &[]);
        let layout = layout(&dir);
        seed_build(&layout, "ay", 16, false);
        seed_build(&layout, "ay", 17, false);
        seed_build(&layout, "ay", 18, true); // active
        let r = rollback(&fake, &layout, &anchor(), "stable", "ay", fl(0), 0).unwrap();
        assert_eq!(r.from_build, 18);
        assert_eq!(r.to_build, 17);
        assert_eq!(
            crate::ops::which(&layout, "ay").unwrap(),
            tool_bin(&layout.build_dir("ay", 17), "ay")
        );
        assert_eq!(
            std::fs::read_link(layout.channel_current("stable")).unwrap(),
            layout.build_dir("ay", 17)
        );
        // The rollback VERB reads the alias policy off the index it just verified — ay is
        // listed with no `system` and no `extra`, so `alab-ay` stands beside the restored
        // `ay`, pointing where it points (the fixture seeded 18 without one; the pass
        // would have laid it since).
        assert_eq!(
            crate::ops::which(&layout, "alab-ay"),
            crate::ops::which(&layout, "ay"),
            "the alias is restored with its primary"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE DISARM THAT WAS ONE `aterm pkg rollback trust` AWAY (m3, 2026-09-17).
    ///
    /// `store/trust/8595/` held 417 MB of orphaned `lib/`, no `bin/` at all, and a
    /// `8595.ready` beside it still saying `ok`. Selection reads `list_installed`, which
    /// counts a build on the strength of that marker, so 8595 was the highest retained build
    /// below current and therefore the rollback target. `rollback_member` then asks
    /// `target.exists()` of every tool and takes the `else` arm — `remove_file(shim)` — for
    /// every one of them: the verb reports a successful rollback and the machine is left
    /// with no compiler, no verifier and no `ay`.
    ///
    /// The marker is now refuted by the tree it vouches for, and corpses like this one can
    /// no longer be made — but a build already on disk carries a marker written by a version
    /// that recorded nothing, so selection asks the tree directly as well. The fixture
    /// forges exactly that: a LEGACY marker (a bare `ok\n`, which every older atpkg wrote)
    /// over a gutted tree.
    #[test]
    fn rollback_refuses_a_target_whose_tools_are_gone_and_takes_the_one_below() {
        let dir = scratch("rollback-gutted");
        let layout = layout(&dir);
        let fake = rollback_index(0, &[]);
        seed_build(&layout, "ay", 16, false);
        seed_build(&layout, "ay", 17, false);
        seed_build(&layout, "ay", 18, true); // active
        // Gut 17 the way an interrupted removal does, and re-mark it the way every atpkg
        // before the contents record did: a bare `ok`, vouching for a tree with no `bin/`.
        std::fs::remove_dir_all(layout.build_dir("ay", 17).join("bin")).unwrap();
        std::fs::write(
            layout.build_dir("ay", 17).with_file_name("17.ready"),
            b"ok\n",
        )
        .unwrap();
        assert!(
            crate::ops::list_installed(&layout).contains(&("ay".to_string(), 17)),
            "the fixture must leave 17 looking installed, or it proves nothing"
        );

        let r = rollback(&fake, &layout, &anchor(), "stable", "ay", fl(0), 0).unwrap();
        assert_eq!(
            r.to_build, 16,
            "the gutted build is skipped and the rollback lands on one that can run"
        );
        assert_eq!(
            crate::ops::which(&layout, "ay").unwrap(),
            tool_bin(&layout.build_dir("ay", 16), "ay"),
            "and the shim points at a binary that is actually there"
        );

        // …and when the gutted build is the ONLY candidate, the verb REFUSES rather than
        // disarming the machine. Nothing may be removed on the way to that refusal.
        std::fs::remove_dir_all(layout.build_dir("ay", 16).join("bin")).unwrap();
        std::fs::write(
            layout.build_dir("ay", 16).with_file_name("16.ready"),
            b"ok\n",
        )
        .unwrap();
        crate::activate::activate_channel(&layout, "stable", &layout.build_dir("ay", 18)).unwrap();
        crate::activate::install_shims(
            &layout,
            &layout.build_dir("ay", 18),
            &["ay".to_string()],
            crate::activate::Aliases::Off,
        )
        .unwrap();
        let err = rollback(&fake, &layout, &anchor(), "stable", "ay", fl(0), 0)
            .expect_err("no landable build below current");
        assert!(
            err.to_string().contains("still holds the tools"),
            "the refusal says WHY, not just that the gate failed: {err}"
        );
        assert_eq!(
            crate::ops::which(&layout, "ay").unwrap(),
            tool_bin(&layout.build_dir("ay", 18), "ay"),
            "a refused rollback leaves the live shim exactly where it was"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rollback_respects_floor_and_yank() {
        // min_build=17: 16 is below the floor, so from 18 => 17.
        {
            let dir = scratch("rollback-floor");
            let lay = layout(&dir);
            seed_build(&lay, "ay", 16, false);
            seed_build(&lay, "ay", 17, false);
            seed_build(&lay, "ay", 18, true);
            let r = rollback(
                &rollback_index(17, &[]),
                &lay,
                &anchor(),
                "stable",
                "ay",
                fl(0),
                0,
            )
            .unwrap();
            assert_eq!(r.to_build, 17);
            let _ = std::fs::remove_dir_all(&dir);
        }
        // yanked ay@17 => skip 17, land on 16.
        {
            let dir = scratch("rollback-yank");
            let lay = layout(&dir);
            seed_build(&lay, "ay", 16, false);
            seed_build(&lay, "ay", 17, false);
            seed_build(&lay, "ay", 18, true);
            let r = rollback(
                &rollback_index(0, &["ay@17"]),
                &lay,
                &anchor(),
                "stable",
                "ay",
                fl(0),
                0,
            )
            .unwrap();
            assert_eq!(r.to_build, 16);
            let _ = std::fs::remove_dir_all(&dir);
        }
        // min_build=18: no retained build below current qualifies => Err(Rollback).
        {
            let dir = scratch("rollback-none-floor");
            let lay = layout(&dir);
            seed_build(&lay, "ay", 16, false);
            seed_build(&lay, "ay", 17, false);
            seed_build(&lay, "ay", 18, true);
            let err = rollback(
                &rollback_index(18, &[]),
                &lay,
                &anchor(),
                "stable",
                "ay",
                fl(0),
                0,
            )
            .unwrap_err();
            assert!(matches!(err, FlowError::Rollback(_)), "got {err:?}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    // REGRESSION (audit 2026-09-15): the rollback target predicate must read THIS program's
    // floor, not the channel-wide number — a floor published for one program must neither
    // refuse another's retained builds nor be ignored for the program it names.
    #[test]
    fn rollback_reads_the_floor_of_the_program_it_rolls_back() {
        // Floored at 18 for `ay` ITSELF (channel-wide floor 0): nothing below the active
        // build qualifies, so the rollback errors rather than landing on a revoked build.
        {
            let dir = scratch("rollback-prog-floor");
            let lay = layout(&dir);
            seed_build(&lay, "ay", 16, false);
            seed_build(&lay, "ay", 17, false);
            seed_build(&lay, "ay", 18, true);
            let err = rollback(
                &rollback_index_per_program(18),
                &lay,
                &anchor(),
                "stable",
                "ay",
                fl(0),
                0,
            )
            .unwrap_err();
            assert!(matches!(err, FlowError::Rollback(_)), "got {err:?}");
            let _ = std::fs::remove_dir_all(&dir);
        }
        // Floored at 17: 16 is below `ay`'s floor, so from 18 the rollback lands on 17.
        {
            let dir = scratch("rollback-prog-floor-lands");
            let lay = layout(&dir);
            seed_build(&lay, "ay", 16, false);
            seed_build(&lay, "ay", 17, false);
            seed_build(&lay, "ay", 18, true);
            let r = rollback(
                &rollback_index_per_program(17),
                &lay,
                &anchor(),
                "stable",
                "ay",
                fl(0),
                0,
            )
            .unwrap();
            assert_eq!(r.to_build, 17);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn rollback_errors_when_no_lower_build() {
        let dir = scratch("rollback-nolower");
        let fake = rollback_index(0, &[]);
        let layout = layout(&dir);
        seed_build(&layout, "ay", 18, true); // only 18 present
        let err = rollback(&fake, &layout, &anchor(), "stable", "ay", fl(0), 0).unwrap_err();
        assert!(matches!(err, FlowError::Rollback(_)), "got {err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The step-11 fix: an `update <grouped-member>` routes through apply_program, which moves
    // the WHOLE tuple atomically — a grouped member can never move alone.
    #[test]
    fn apply_program_moves_a_grouped_member_as_a_tuple() {
        let dir = scratch("apply-program");
        let fake = group_fixture(&dir);
        let layout = layout(&dir);
        seed_build(&layout, "ay", 17, true); // ay@17 active, grouped member installed
        let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
        let report = apply_program(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            "ay",
            &installed,
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert_eq!(report.groups.len(), 1, "the one group containing ay");
        assert_eq!(
            report.groups[0].1,
            TxnOutcome::Applied(vec!["ay".into(), "trust".into()])
        );
        assert_eq!(
            crate::ops::which(&layout, "ay").unwrap(),
            tool_bin(&layout.build_dir("ay", 18), "ay")
        );
        assert_eq!(
            crate::ops::which(&layout, "trust").unwrap(),
            tool_bin(&layout.build_dir("trust", 4821), "trust"),
            "the locked sibling moved too — the member could not move alone"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- requires dependency pull-in (step 17) -----------------------------------------

    /// A signed release naming `ay`@18 + `ny`@9 (NO coherence group), with configurable
    /// per-program `requires` and channel `yanked`.
    fn requires_fixture(
        dir: &Path,
        yanked: &[&str],
        ay_requires: &[&str],
        ny_requires: &[&str],
    ) -> Fake {
        requires_fixture_with(dir, yanked, ay_requires, ny_requires, false)
    }

    /// [`requires_fixture`] with `ny` optionally an EXTRA (`extra = true` in the index).
    fn requires_fixture_with(
        dir: &Path,
        yanked: &[&str],
        ay_requires: &[&str],
        ny_requires: &[&str],
        ny_extra: bool,
    ) -> Fake {
        fn req_line(reqs: &[&str]) -> String {
            if reqs.is_empty() {
                String::new()
            } else {
                let items: Vec<String> = reqs.iter().map(|r| format!("\"{r}\"")).collect();
                format!("requires = [{}]\n", items.join(", "))
            }
        }
        let mut pkg = HashMap::new();
        let mut archives = HashMap::new();
        for (program, build, reqs) in [("ay", 18u64, ay_requires), ("ny", 9u64, ny_requires)] {
            let archive = prog_archive(dir, program, build);
            let sha = crate::tree::file_sha256(&archive).unwrap();
            let probe = dir.join(format!("probe-{program}"));
            let _ = std::fs::remove_dir_all(&probe);
            crate::extract::extract_tar_zst(&archive, &probe, 10_000_000, 10_000).unwrap();
            let root = crate::tree::tree_root(&probe).unwrap();
            let asset = format!("{program}-{build}.tar.zst");
            let pkg_body = format!(
                "schema = 2\nprogram = \"{program}\"\nversion = \"0.1\"\nbuild_number = {build}\n\
                 exposes = [\"{program}\"]\n{reqs}\
                 [[artifact]]\ntarget = \"{TRIPLE}\"\nkind = \"binary\"\nasset = \"{asset}\"\n\
                 sha256 = \"{sha}\"\ntree_root = \"{root}\"\nsize = 100\n\
                 [artifact.cost]\ndisk_installed = 1048576\n",
                reqs = req_line(reqs)
            );
            pkg.insert(
                (program.to_string(), build),
                (
                    pkg_body.clone().into_bytes(),
                    sign(&RELEASE_SEED, pkg_body.as_bytes()),
                ),
            );
            archives.insert(asset, archive);
        }
        let yanked_toml = if yanked.is_empty() {
            String::new()
        } else {
            let items: Vec<String> = yanked.iter().map(|y| format!("\"{y}\"")).collect();
            format!("yanked = [{}]\n", items.join(", "))
        };
        let index_body = format!(
            "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n{attr}\
                          [programs.ay]\nrepo = \"ay\"\n[programs.ny]\nrepo = \"ny\"\n{extra}\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             {yanked_toml}pin = {{ ay = 18, ny = 9 }}\n",
            attr = attribution(),
            extra = if ny_extra { "extra = true\n" } else { "" }
        );
        Fake {
            index: index_body.clone().into_bytes(),
            index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
            pkg,
            archives,
        }
    }

    /// THE UPDATE LANE IS GATED TOO (§17.10): `ay` requires `ny`; ay is installed at an
    /// old build and ny is not installed at all (uninstalled, or never there). The pass
    /// holds ay on its current build — `TxnOutcome::Blocked` naming ny and quoting its
    /// state — and resolves nothing for it: no artifact selected, no byte staged. Once ny
    /// is live, the same pass shape moves ay to its pin. The update lane never installs a
    /// MISSING dependency itself (that is the set-completion pass's job), so ny's own
    /// group is simply not an update.
    #[test]
    fn apply_channel_holds_an_installed_dependent_whose_requirement_is_unmet() {
        let dir = scratch("update-gate");
        // The edge rides the INDEX (`[programs.ay].requires`) — the relation the plan
        // and the gate read, known before any manifest is fetched — not the pkg
        // manifest's own `requires`, which `install` unions in at install time.
        let fake = requires_fixture(&dir, &[], &[], &[]);
        let index_body = String::from_utf8(fake.index.clone()).unwrap().replace(
            "[programs.ay]\nrepo = \"ay\"\n",
            "[programs.ay]\nrepo = \"ay\"\nrequires = [\"ny\"]\n",
        );
        assert!(index_body.contains("requires = [\"ny\"]"), "{index_body}");
        let fake = Fake {
            index: index_body.clone().into_bytes(),
            index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
            pkg: fake.pkg,
            archives: fake.archives,
        };
        let layout = layout(&dir);
        seed_build(&layout, "ay", 17, true);
        let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
        let report = apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &installed,
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert_eq!(
            report.groups.len(),
            1,
            "ny is not installed, so it is not an update: {:?}",
            report.groups
        );
        let (group, outcome) = &report.groups[0];
        assert_eq!(group.members, vec!["ay".to_string()]);
        assert_eq!(
            *outcome,
            TxnOutcome::Blocked {
                dep: "ny".into(),
                dep_state: "not installed".into()
            }
        );
        assert!(report.applied.is_empty(), "nothing flipped");
        assert!(
            !report.resolved_assets.contains_key("ay"),
            "nothing resolved for a held group"
        );
        assert!(!layout.build_dir("ay", 18).exists(), "nothing staged");
        assert_eq!(
            crate::ops::active_builds(&layout).get("ay").copied(),
            Some(17),
            "held on its current build"
        );
        // The requirement met: the dependent moves.
        seed_build(&layout, "ny", 9, true);
        let installed = crate::ops::active_builds(&layout);
        let report = apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &installed,
            &[],
            fl(0),
            0,
        )
        .unwrap();
        let ay = report
            .groups
            .iter()
            .find(|(g, _)| g.members == vec!["ay".to_string()])
            .expect("ay's group");
        assert_eq!(ay.1, TxnOutcome::Applied(vec!["ay".into()]));
        assert_eq!(
            crate::ops::active_builds(&layout).get("ay").copied(),
            Some(18)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn requires_pulls_in_missing_dep() {
        let dir = scratch("req-pull");
        let fake = requires_fixture(&dir, &[], &["ny"], &[]);
        let layout = layout(&dir);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let report = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
        assert_eq!(report.build, 18);
        assert!(
            report
                .dependencies
                .iter()
                .any(|d| d.program == "ny"
                    && matches!(d.result, DepResult::Installed { build: 9, .. })),
            "ny pulled in first: {:?}",
            report.dependencies
        );
        assert!(crate::ops::which(&layout, "ay").is_some());
        assert!(
            crate::ops::which(&layout, "ny").is_some(),
            "the dep is live too"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A dependency that is an EXTRA is never opted in on the dependent's behalf: without
    /// this machine's opt-in marker it is Skipped with the consent spelling and no byte
    /// of it moves; with the marker it is pulled in like any dependency.
    #[test]
    fn requires_never_opts_in_to_an_extra_on_the_dependents_behalf() {
        let dir = scratch("req-extra");
        let fake = requires_fixture_with(&dir, &[], &["ny"], &[], true);
        let layout = layout(&dir);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let report = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
        assert!(
            crate::ops::which(&layout, "ay").is_some(),
            "ay still installs"
        );
        let ny = report
            .dependencies
            .iter()
            .find(|d| d.program == "ny")
            .expect("ny is named");
        assert!(
            matches!(&ny.result, DepResult::Skipped(why) if why == &crate::state::extra_not_installed("ny")),
            "{:?}",
            ny.result
        );
        assert!(crate::ops::which(&layout, "ny").is_none());
        assert!(
            !layout.staging_dir("ny").exists() && !layout.build_dir("ny", 9).exists(),
            "no byte of the extra moved"
        );
        assert!(!layout.optin_exists("ny"), "nothing opted in for the user");
        // Opted in by the user: an ordinary dependency.
        layout.record_optin("ny").unwrap();
        let again = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let report = install(&fake, &layout, &anchor(), &again, fl(0), 0).unwrap();
        assert!(
            report
                .dependencies
                .iter()
                .any(|d| d.program == "ny"
                    && matches!(d.result, DepResult::Installed { build: 9, .. })),
            "{:?}",
            report.dependencies
        );
        assert!(crate::ops::which(&layout, "ny").is_some());
        // The alias policy through the REAL pipeline (§17.11): ay is ALab's own (no
        // `system`, not an extra) and gets `alab-ay` beside `ay`; ny is a vendor EXTRA
        // and never gets one — even though it is installed through the same lane.
        assert!(
            crate::ops::which(&layout, "alab-ay")
                .is_some_and(|t| t.starts_with(layout.build_dir("ay", 18))),
            "an ALab program's alias is laid by the install"
        );
        assert!(
            std::fs::symlink_metadata(layout.shim(&tool("alab-ny"))).is_err(),
            "no alias for a vendor extra"
        );
        assert_eq!(Aliases::laid_for(&layout, "ny"), Aliases::Off);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn requires_skips_yanked_dep_with_warning() {
        let dir = scratch("req-yank");
        let fake = requires_fixture(&dir, &["ny@9"], &["ny"], &[]);
        let layout = layout(&dir);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let report = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
        assert!(
            crate::ops::which(&layout, "ay").is_some(),
            "ay still installs"
        );
        assert!(
            report
                .dependencies
                .iter()
                .any(|d| d.program == "ny" && matches!(d.result, DepResult::Skipped(_))),
            "a yanked dep is Skipped, not fatal: {:?}",
            report.dependencies
        );
        assert!(
            crate::ops::which(&layout, "ny").is_none(),
            "the gate is NOT bypassed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn requires_skips_unreachable_dep() {
        let dir = scratch("req-unreach");
        let fake = requires_fixture(&dir, &[], &["ghost"], &[]);
        let layout = layout(&dir);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let report = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
        assert!(crate::ops::which(&layout, "ay").is_some());
        assert!(
            report
                .dependencies
                .iter()
                .any(|d| d.program == "ghost" && matches!(d.result, DepResult::Skipped(_))),
            "an unlisted dep is Skipped (requires can't install an unlisted repo)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn requires_already_present_dep_not_reinstalled() {
        let dir = scratch("req-present");
        let fake = requires_fixture(&dir, &[], &["ny"], &[]);
        let layout = layout(&dir);
        // Install ny first.
        let ny = InstallRequest {
            channel: "stable",
            program: "ny",
            triple: TRIPLE,
            installed: None,
        };
        install(&fake, &layout, &anchor(), &ny, fl(0), 0).unwrap();
        // Now ay requires ny, which is already active.
        let ay = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let report = install(&fake, &layout, &anchor(), &ay, fl(0), 0).unwrap();
        assert!(
            report
                .dependencies
                .iter()
                .any(|d| d.program == "ny" && d.result == DepResult::AlreadyPresent(9)),
            "an already-active dep is AlreadyPresent, not reinstalled"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn requires_cycle_terminates() {
        let dir = scratch("req-cycle");
        // ay requires ny AND ny requires ay.
        let fake = requires_fixture(&dir, &[], &["ny"], &["ay"]);
        let layout = layout(&dir);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let report = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
        // No infinite recursion: ay completes, both active, the back-edge is a cycle skip.
        assert!(crate::ops::which(&layout, "ay").is_some());
        assert!(crate::ops::which(&layout, "ny").is_some());
        assert!(
            report
                .dependencies
                .iter()
                .any(|d| d.program == "ay" && matches!(d.result, DepResult::Skipped(_))),
            "the back-edge ny→ay is recorded as a cycle skip"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_report_carries_signed_tree_root() {
        let dir = scratch("req-treeroot");
        let fake = fixture(&dir);
        let layout = layout(&dir);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let report = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap();
        assert!(
            !report.tree_root.is_empty(),
            "the signed tree_root is recorded"
        );
        assert_eq!(
            report.tree_root,
            crate::tree::tree_root(&layout.build_dir("ay", 18)).unwrap(),
            "it equals the on-disk tree's recomputed root (what `atpkg verify` compares)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- dev-link skip + dir fetcher + same-source cache (steps 13/14) ------------------

    /// Mark `program` dev-linked by writing a minimal marker (no real checkout needed to
    /// exercise the is_linked HARD-SKIP).
    fn mark_linked(layout: &Layout, program: &str) {
        std::fs::create_dir_all(layout.links_dir()).unwrap();
        std::fs::write(
            layout.link_marker(program),
            format!(
                "schema = 2\nprogram = \"{program}\"\ncheckout = \"/nonexistent\"\nbins = []\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn linked_program_is_hard_skipped_by_install() {
        let dir = scratch("linked-skip");
        let fake = fixture(&dir);
        let layout = layout(&dir);
        mark_linked(&layout, "ay");
        assert!(crate::linkmode::is_linked(&layout, "ay"));
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let err = install(&fake, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(matches!(err, FlowError::Linked(_)), "got {err:?}");
        assert!(
            crate::ops::which(&layout, "ay").is_none(),
            "no store shim/build created"
        );
        assert!(!layout.build_dir("ay", 18).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_channel_skips_a_linked_group_member() {
        let dir = scratch("linked-group");
        let fake = group_fixture(&dir);
        let layout = layout(&dir);
        seed_build(&layout, "ay", 17, true); // ay@17 active, grouped member installed
        mark_linked(&layout, "ay");
        let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
        let report = apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &installed,
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert!(
            report.groups.is_empty(),
            "the linked group is excluded from the apply"
        );
        assert!(
            report.skipped_linked.contains(&"ay".to_string()),
            "the linked member is reported"
        );
        assert!(
            !layout.build_dir("trust", 4821).exists(),
            "the locked sibling is untouched"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A [`Fake`] wrapper whose index fetch can be toggled to fail — or to succeed
    /// EMPTY (the pushed-off-the-page listing) — with a controllable `source_id`
    /// (to exercise the same-source cache guard + the empty-success fallback).
    struct FlakyFake {
        inner: Fake,
        fail: std::cell::Cell<bool>,
        empty: std::cell::Cell<bool>,
        source: String,
    }
    impl FlakyFake {
        fn new(inner: Fake, source: &str) -> Self {
            Self {
                inner,
                fail: std::cell::Cell::new(false),
                empty: std::cell::Cell::new(false),
                source: source.into(),
            }
        }
    }
    impl Fetcher for FlakyFake {
        fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
            if self.fail.get() {
                Err("network down".into())
            } else if self.empty.get() {
                Ok(vec![])
            } else {
                self.inner.index_candidates()
            }
        }
        fn pkg_manifest(
            &self,
            repo: &str,
            program: &str,
            build: u64,
        ) -> Result<(Vec<u8>, Vec<u8>), String> {
            self.inner.pkg_manifest(repo, program, build)
        }
        fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
            self.inner.download(repo, asset, dest)
        }
        fn source_id(&self) -> String {
            self.source.clone()
        }
    }

    #[test]
    fn cached_index_is_used_only_on_same_source_fetch_failure() {
        let dir = scratch("cache-fallback");
        let layout = layout(&dir);
        let f = FlakyFake::new(fixture(&dir), "src:A");
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        // 1. A good fetch installs AND caches the index under source "src:A".
        install(&f, &layout, &anchor(), &req, fl(0), 0).unwrap();
        // 2. Fetch now fails, SAME source → the install is served from the cache.
        f.fail.set(true);
        install(&f, &layout, &anchor(), &req, fl(0), 0).expect("cache fallback serves the index");
        // 3. A DIFFERENT source with a failing fetch has no cache to fall back on.
        //    The error must name TRANSPORT, not trust: this is the state an offline,
        //    proxied or rate-limited machine reaches, and reporting it as
        //    "no signature-valid index" sent those users to key management when the
        //    fix was to connect to the network.
        let f2 = FlakyFake::new(fixture(&dir), "src:B");
        f2.fail.set(true);
        let err = install(&f2, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(
            matches!(err, FlowError::Unreachable(_)),
            "a dir: cache never satisfies a github: fetch, and an unreachable source \
             must not be reported as a signature failure — got {err:?}"
        );
        let rendered = err.to_string();
        assert!(
            rendered.contains("network problem") && !rendered.contains("signature-valid"),
            "the message must point at the network: {rendered}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A [`Fake`] whose index listing answers GitHub's anonymous rate limit — the exact
    /// error string `aterm_update_core`'s API layer renders for a 403/429 on the LIST,
    /// which is what a drained office IP hands `atpkg` at GUI launch.
    struct RateLimitedFake {
        inner: Fake,
        limited: std::cell::Cell<bool>,
        source: String,
    }
    impl Fetcher for RateLimitedFake {
        fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
            if self.limited.get() {
                Err(aterm_update_core::HttpError::RateLimited {
                    code: 403,
                    url: "https://api.github.com/repos/alabsystems/atpkg-index/releases?per_page=100&page=1".into(),
                    authenticated: false,
                }
                .to_string())
            } else {
                self.inner.index_candidates()
            }
        }
        fn pkg_manifest(
            &self,
            repo: &str,
            program: &str,
            build: u64,
        ) -> Result<(Vec<u8>, Vec<u8>), String> {
            self.inner.pkg_manifest(repo, program, build)
        }
        fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
            self.inner.download(repo, asset, dest)
        }
        fn source_id(&self) -> String {
            self.source.clone()
        }
    }

    /// A RATE-LIMITED listing is a transport failure like any other: the same-source
    /// identity cache stands in for it, and without a cache the verdict is
    /// `Unreachable` naming the rate limit — never `NoIndex`, never a signature failure.
    /// This is the outcome a drained IP at GUI launch reaches, and the cache is what
    /// keeps the toolchain usable through it.
    #[test]
    fn a_rate_limited_listing_is_unreachable_so_the_cache_stands_in() {
        let dir = scratch("cache-rate-limited");
        let layout = layout(&dir);
        let f = RateLimitedFake {
            inner: fixture(&dir),
            limited: std::cell::Cell::new(false),
            source: "src:A".into(),
        };
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        install(&f, &layout, &anchor(), &req, fl(0), 0).unwrap();
        f.limited.set(true);
        install(&f, &layout, &anchor(), &req, fl(0), 0)
            .expect("a rate-limited listing is served from the same-source cache");
        let f2 = RateLimitedFake {
            inner: fixture(&dir),
            limited: std::cell::Cell::new(true),
            source: "src:B".into(),
        };
        let err = install(&f2, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(
            matches!(err, FlowError::Unreachable(_)),
            "a rate limit with no cache is a transport verdict, not a trust one: {err:?}"
        );
        let rendered = err.to_string();
        assert!(
            rendered.contains("rate limit")
                && rendered.contains("network problem")
                && !rendered.contains("signature-valid"),
            "the message names the rate limit and points at the network: {rendered}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // An EMPTY listing success is a fetch that FOUND nothing — the index tag pushed off
    // the release page by app-release cadence, a repo with no index release — and must
    // take the SAME §14 same-source fallback as a hard Err. Before the fix, Ok(empty)
    // bypassed the fallback into a repo-wide NoIndex while a good cache sat on disk.
    #[test]
    fn empty_candidates_success_takes_the_same_source_cache_fallback() {
        let dir = scratch("cache-empty");
        let layout = layout(&dir);
        let f = FlakyFake::new(fixture(&dir), "src:A");
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        // 1. A good fetch installs AND caches under "src:A".
        install(&f, &layout, &anchor(), &req, fl(0), 0).unwrap();
        // 2. The listing now succeeds EMPTY, SAME source → served from the cache.
        f.empty.set(true);
        install(&f, &layout, &anchor(), &req, fl(0), 0)
            .expect("an empty success falls back to the last-good cache");
        // 3. Empty success + no cache (fresh store, different source) → NoIndex.
        let dir2 = scratch("cache-empty-fresh");
        let fresh_layout = super::tests::layout(&dir2);
        let f2 = FlakyFake::new(fixture(&dir2), "src:B");
        f2.empty.set(true);
        let err = install(&f2, &fresh_layout, &anchor(), &req, fl(0), 0).unwrap_err();
        assert!(matches!(err, FlowError::NoIndex), "got {err:?}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    // §14 CACHE KEYED OFF THE NETWORK LEG ONLY (the cache-masking tooth, 2026-07-30):
    // chaining a seed dir must not let a seed-leg success rewrite the cache. Network
    // leg DOWN → the resolve succeeds from the seed but writes NO cache; network leg
    // UP → the cache holds the NETWORK candidates under the NETWORK source id, so the
    // post-seed plain-network path falls back to the very same cache.
    #[test]
    fn chain_cache_is_keyed_off_the_network_leg_only() {
        let dir = scratch("chain-cache");
        let fake = fixture(&dir);
        // The seed leg: the fixture laid out as a dir registry.
        let reg = dir.join("seed-reg");
        std::fs::create_dir_all(&reg).unwrap();
        std::fs::write(reg.join("index.toml"), &fake.index).unwrap();
        std::fs::write(reg.join("index.toml.sig"), &fake.index_sig).unwrap();
        // A `dir:` registry publishes the master-signed roster too, exactly as a release
        // does: index without the generation that authorized its signer is not a registry.
        let (rb, rs) = testkit::published_roster();
        std::fs::write(reg.join(aterm_update_core::roster::ROSTER_ASSET), &rb).unwrap();
        std::fs::write(reg.join(aterm_update_core::roster::ROSTER_SIG_ASSET), &rs).unwrap();
        let (raw, sig) = fake.pkg.get(&("ay".to_string(), 18u64)).unwrap();
        std::fs::write(reg.join("pkg-ay-18.toml"), raw).unwrap();
        std::fs::write(reg.join("pkg-ay-18.toml.sig"), sig).unwrap();
        std::fs::copy(
            fake.archives.get("ay-18.tar.zst").unwrap(),
            reg.join("ay-18.tar.zst"),
        )
        .unwrap();
        let layout = layout(&dir);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let cache = crate::cache::IndexCache::for_layout(&layout);
        // 1. Network DOWN, seed serves: the install succeeds via the seed leg…
        let down = FlakyFake::new(fixture(&dir), "github:t/aterm");
        down.fail.set(true);
        let chain = crate::net::ChainFetcher::new(
            Box::new(down),
            Box::new(crate::net::DirFetcher::new(reg.clone())),
        );
        install(&chain, &layout, &anchor(), &req, fl(0), 0)
            .expect("the seed leg serves the bootstrap");
        // …but writes NO cache: a seed success must not mask the network failure.
        assert!(
            cache.load("github:t/aterm").is_none(),
            "no network cache from a seed-leg success"
        );
        assert!(
            cache.load(&chain.source_id()).is_none(),
            "no chain-id cache either"
        );
        // 2. Network UP: the cache holds the NETWORK leg's candidates, network id.
        let chain_up = crate::net::ChainFetcher::new(
            Box::new(FlakyFake::new(fixture(&dir), "github:t/aterm")),
            Box::new(crate::net::DirFetcher::new(reg.clone())),
        );
        install(&chain_up, &layout, &anchor(), &req, fl(0), 0).unwrap();
        let cached = cache
            .load("github:t/aterm")
            .expect("network candidates cached under the NETWORK id");
        assert_eq!(cached.len(), 1, "the seed leg's candidate is not absorbed");
        assert_eq!(
            cached[0].label, "v0",
            "the network leg's candidate, not the dir leg's"
        );
        // 3. The plain-network path (same id, seed no longer chained) falls back to it.
        let plain = FlakyFake::new(fixture(&dir), "github:t/aterm");
        plain.fail.set(true);
        install(&plain, &layout, &anchor(), &req, fl(0), 0)
            .expect("the §14 fallback serves the plain-network path from the chain-written cache");

        // 4. THE READ HALF of the cache-masking tooth. With the network leg DOWN and
        //    the seed leg answering, the resolve must still CONSULT the last-good
        //    network cache — not silently accept the seal as the only word.
        //
        //    Guarding only the cache WRITE (steps 1-3) left this open: a seed-leg
        //    success turned the network failure into `Ok`, and the cache was read
        //    exclusively in the failure arm, so the cached index was never even
        //    looked at. On an empty store — the only state the seed leg is chained
        //    in — the durable index_build floor cannot rise, so nothing else would
        //    have caught a seal that reinstated pins a newer cached index had
        //    yanked or floored out.
        let down_again = FlakyFake::new(fixture(&dir), "github:t/aterm");
        down_again.fail.set(true);
        let chain_down = crate::net::ChainFetcher::new(
            Box::new(down_again),
            Box::new(crate::net::DirFetcher::new(reg.clone())),
        );
        let resolved = resolve_candidates(&chain_down, &layout)
            .expect("the seed leg still answers when the network is down");
        let labels: Vec<&str> = resolved.iter().map(|c| c.label.as_str()).collect();
        assert!(
            labels.contains(&"v0"),
            "the last-good NETWORK candidate must be unioned in, not masked by the \
             seed-leg success — got {labels:?}"
        );
        assert!(
            labels.contains(&"dir"),
            "the seed's own candidate must still be offered — got {labels:?}"
        );
        assert_eq!(
            labels[0], "v0",
            "cached network candidates come FIRST: select_index replaces only on a \
             strictly greater index_build, so a tie must go to the network's last-good \
             index rather than the seal — got {labels:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A [`Fake`] that COUNTS index fetches and answers the cheap identity probe from a
    /// cell, so a test can move one asset's fingerprint and watch the resolve react.
    struct IdentityFake {
        inner: Fake,
        fetches: std::cell::Cell<u32>,
        identity: std::cell::RefCell<Vec<String>>,
    }
    impl IdentityFake {
        fn new(inner: Fake, identity: &[&str]) -> Self {
            Self {
                inner,
                fetches: std::cell::Cell::new(0),
                identity: std::cell::RefCell::new(
                    identity.iter().map(|s| (*s).to_string()).collect(),
                ),
            }
        }
    }
    impl Fetcher for IdentityFake {
        fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
            self.fetches.set(self.fetches.get().saturating_add(1));
            self.inner.index_candidates()
        }
        fn pkg_manifest(
            &self,
            repo: &str,
            program: &str,
            build: u64,
        ) -> Result<(Vec<u8>, Vec<u8>), String> {
            self.inner.pkg_manifest(repo, program, build)
        }
        fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
            self.inner.download(repo, asset, dest)
        }
        fn source_id(&self) -> String {
            "src:identity".to_string()
        }
        fn index_identities(&self) -> Option<Vec<String>> {
            Some(self.identity.borrow().clone())
        }
    }

    /// THE HIT PATH, end to end and two-sided. A resolve whose cheap identity probe
    /// matches the §14 cache must do NO index fetch at all and must hand selection
    /// byte-identical candidates; the moment an identity moves — a re-uploaded asset, a
    /// new carrying release — the fetch comes straight back.
    ///
    /// The counter is the instrument. Without it this optimization is unfalsifiable: a
    /// resolve that quietly re-downloaded everything would still pass every functional
    /// assertion, which is exactly how a "cache" ends up never being a hit path (the
    /// state this crate was in — `resolve_candidates` consulted the cache only when the
    /// fetch had already failed).
    #[test]
    fn a_matching_identity_serves_the_cache_with_no_index_fetch() {
        let dir = scratch("identity-hit");
        let layout = layout(&dir);
        let f = IdentityFake::new(fixture(&dir), &["id-v0"]);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        // 1. COLD. The install fetches for real and caches the candidates WITH their
        //    identity.
        install(&f, &layout, &anchor(), &req, fl(0), 0).unwrap();
        let cold = f.fetches.get();
        assert!(cold >= 1, "the cold pass must actually fetch — got {cold}");

        // 2. WARM, same identity: zero fetches, and the SAME bytes reach selection.
        let got = resolve_candidates(&f, &layout).expect("the warm cache resolves");
        assert_eq!(
            f.fetches.get(),
            cold,
            "a matching identity must skip index_candidates entirely"
        );
        let fresh = f.inner.index_candidates().expect("fixture candidates");
        assert_eq!(got.len(), fresh.len(), "same candidate set");
        assert_eq!(got[0].label, fresh[0].label);
        assert_eq!(
            got[0].index_bytes, fresh[0].index_bytes,
            "the cache serves the very bytes the source publishes"
        );
        assert_eq!(got[0].sig, fresh[0].sig);
        assert_eq!(
            got[0].roster_bytes, fresh[0].roster_bytes,
            "the roster rides with its index through the hit path too"
        );
        assert_eq!(got[0].roster_sig, fresh[0].roster_sig);

        // 3. THE OTHER SIDE. Move the identity (an asset re-uploaded, a release cut) and
        //    the resolve must go back to the network — otherwise this seam would be a
        //    permanent downgrade oracle rather than a cache.
        f.identity.borrow_mut()[0] = "id-v1".to_string();
        let after_move = f.fetches.get();
        resolve_candidates(&f, &layout).expect("a moved identity re-fetches");
        assert_eq!(
            f.fetches.get(),
            after_move + 1,
            "a changed identity must re-download"
        );

        // 4. And a COUNT change (a newly published carrying release) refuses too.
        f.identity.borrow_mut().push("id-extra".to_string());
        let before_grow = f.fetches.get();
        resolve_candidates(&f, &layout).expect("a grown candidate set re-fetches");
        assert_eq!(
            f.fetches.get(),
            before_grow + 1,
            "a different candidate COUNT must re-download"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fetcher that answers the identity probe still has NOTHING to serve without a
    /// same-source cache: the probe is a permission to reuse, never a source of bytes.
    #[test]
    fn an_identity_without_a_cache_still_fetches() {
        let dir = scratch("identity-cold");
        let layout = layout(&dir);
        let f = IdentityFake::new(fixture(&dir), &["id-v0"]);
        let got = resolve_candidates(&f, &layout).expect("cold resolve");
        assert_eq!(f.fetches.get(), 1, "no cache ⇒ the fetch must happen");
        assert_eq!(got.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE SAME HIT PATH, on the lanes a TYPED single-program verb takes. `plan_update`,
    /// `apply_program` and `rollback` resolve their candidates without the §14 failure-time
    /// fallback — deliberately — but that rule is about FAILURES, and it also cost them the
    /// identity hit for as long as the hit path existed: every `aterm pkg update <program>`
    /// in a fresh process re-downloaded the whole candidate set (sixteen assets on the
    /// production fetcher) to obtain bytes the last pass had already stamped as identical.
    ///
    /// The counter is again the instrument, and the test is two-sided: a matching identity
    /// must cost NO index fetch on any of the three, and a moved identity must bring the
    /// fetch straight back — a lane that went on serving a cache the source had moved past
    /// would be the defect in the other direction.
    #[test]
    fn the_typed_single_program_lanes_take_the_hit_path_too() {
        let dir = scratch("identity-lanes");
        let layout = layout(&dir);
        let f = IdentityFake::new(fixture(&dir), &["id-v0"]);
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        // COLD: the install fetches for real and stamps the cache with its identity.
        install(&f, &layout, &anchor(), &req, fl(0), 0).unwrap();
        let installed = crate::ops::active_builds(&layout);
        assert_eq!(installed.get("ay").copied(), Some(18), "ay@18 is active");
        let warm = f.fetches.get();

        // 1. `plan_update` — the first thing a typed `aterm pkg update <program>` calls.
        let plan = plan_update(
            &f,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            "ay",
            Some(18),
            fl(0),
            0,
        )
        .expect("the warm cache plans");
        assert_eq!(plan.decision, ApplyDecision::UpToDate);
        assert_eq!(
            f.fetches.get(),
            warm,
            "plan_update must serve a matching identity from the cache"
        );

        // 2. `apply_program` — the transactional lane, on the same fetcher.
        apply_program(
            &f,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            "ay",
            &installed,
            &[],
            fl(0),
            0,
        )
        .expect("the warm cache applies");
        assert_eq!(
            f.fetches.get(),
            warm,
            "apply_program must serve a matching identity from the cache"
        );

        // 3. `rollback` — it errors (nothing retained below 18), but only AFTER the resolve
        //    this test is about.
        let err = rollback(&f, &layout, &anchor(), "stable", "ay", fl(0), 0).unwrap_err();
        assert!(matches!(err, FlowError::Rollback(_)), "got {err:?}");
        assert_eq!(
            f.fetches.get(),
            warm,
            "rollback must serve a matching identity from the cache"
        );

        // THE OTHER SIDE: move the identity and every lane goes back to the source.
        f.identity.borrow_mut()[0] = "id-v1".to_string();
        let moved = f.fetches.get();
        plan_update(
            &f,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            "ay",
            Some(18),
            fl(0),
            0,
        )
        .expect("a moved identity re-fetches");
        assert_eq!(f.fetches.get(), moved + 1, "plan_update re-downloads");
        let _ = rollback(&f, &layout, &anchor(), "stable", "ay", fl(0), 0);
        assert_eq!(f.fetches.get(), moved + 2, "rollback re-downloads");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_cached_index_is_still_refused() {
        let dir = scratch("cache-stale");
        let layout = layout(&dir);
        let f = FlakyFake::new(fixture(&dir), "src:A");
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        // A `now` past valid_until: even a good fetch is refused Stale — but the bytes are cached.
        assert!(matches!(
            install(&f, &layout, &anchor(), &req, fl(0), 2_000_000_000),
            Err(FlowError::Stale)
        ));
        // The fetch now fails → fallback to the cached bytes, which are STILL past valid_until.
        f.fail.set(true);
        assert!(
            matches!(
                install(&f, &layout, &anchor(), &req, fl(0), 2_000_000_000),
                Err(FlowError::Stale)
            ),
            "freshness still gates cached bytes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dir_fetcher_installs_end_to_end() {
        let dir = scratch("dirfetch-e2e");
        let fake = fixture(&dir);
        let reg = dir.join("registry");
        std::fs::create_dir_all(&reg).unwrap();
        std::fs::write(reg.join("index.toml"), &fake.index).unwrap();
        std::fs::write(reg.join("index.toml.sig"), &fake.index_sig).unwrap();
        // A `dir:` registry publishes the master-signed roster too, exactly as a release
        // does: index without the generation that authorized its signer is not a registry.
        let (rb, rs) = testkit::published_roster();
        std::fs::write(reg.join(aterm_update_core::roster::ROSTER_ASSET), &rb).unwrap();
        std::fs::write(reg.join(aterm_update_core::roster::ROSTER_SIG_ASSET), &rs).unwrap();
        let (raw, sig) = fake.pkg.get(&("ay".to_string(), 18u64)).unwrap();
        std::fs::write(reg.join("pkg-ay-18.toml"), raw).unwrap();
        std::fs::write(reg.join("pkg-ay-18.toml.sig"), sig).unwrap();
        std::fs::copy(
            fake.archives.get("ay-18.tar.zst").unwrap(),
            reg.join("ay-18.tar.zst"),
        )
        .unwrap();
        let layout = layout(&dir);
        let df = crate::net::DirFetcher::new(reg.clone());
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        // dir bytes pass the identical verify + floor + freshness + shim gate.
        let report = install(&df, &layout, &anchor(), &req, fl(0), 0).unwrap();
        assert_eq!(report.build, 18);
        assert_eq!(
            crate::ops::which(&layout, "ay").unwrap(),
            tool_bin(&layout.build_dir("ay", 18), "ay")
        );
        assert!(
            crate::ops::which(&layout, "git").is_none(),
            "sensitive shim refused via dir too"
        );
        // Wrong root key ⇒ NoIndex (verify-before-parse intact even offline).
        let df2 = crate::net::DirFetcher::new(reg);
        let err = install(&df2, &layout, &anchor_of(&RELEASE_SEED), &req, fl(0), 0).unwrap_err();
        assert!(
            matches!(err, FlowError::NoIndex),
            "wrong root refuses the dir index"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A build dir with the given executables, created directly in the store (the
    /// flip/rollback tests below need on-disk shape, not a signed archive).
    fn bare_build(l: &Layout, program: &str, build: u64, bins: &[&str]) -> PathBuf {
        let dir = l.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        for b in bins {
            std::fs::write(tool_bin(&dir, b), b"#!/bin/true\n").unwrap();
        }
        dir
    }

    /// ROLLBACK RESTORES THE PRIOR BUILD'S WHOLE SURFACE. A tool the prior build shipped
    /// and the new build DROPPED has no shim by rollback time — the new build's
    /// `install_tools` prune deleted it (same program, different build: exactly its job) —
    /// so a rollback iterating only the NEW exposes list re-activates the prior build with
    /// that tool missing from PATH. The restore set is the union with the prior `bin/`.
    #[test]
    fn rollback_restores_a_tool_the_new_build_dropped() {
        let dir = scratch("rb-union");
        let l = layout(&dir);
        let b18 = bare_build(&l, "ay", 18, &["ay", "aylint"]);
        let b19 = bare_build(&l, "ay", 19, &["ay"]);
        // 18 live with both tools, then the flip to 19 prunes aylint's stale shim.
        activate_channel(&l, "stable", &b18).unwrap();
        install_tools(&l, &b18, &[tool("ay"), tool("aylint")], Aliases::Off).unwrap();
        activate_channel(&l, "stable", &b19).unwrap();
        install_tools(&l, &b19, &[tool("ay")], Aliases::Off).unwrap();
        assert!(
            crate::platform::resolve_shim(&l.shim(&tool("aylint"))).is_none(),
            "fixture: the prune removed the dropped tool's shim"
        );

        let staged = Staged {
            build: 19,
            build_dir: b19,
            exposes: vec![tool("ay")],
            prior_build: Some(18),
            was_live: false,
            reloc: None,
            aliases: Aliases::Off,
            tree_root: String::new(),
        };
        rollback_member(&l, "stable", "ay", &staged);

        for t in ["ay", "aylint"] {
            let target = crate::platform::resolve_shim(&l.shim(&tool(t)))
                .unwrap_or_else(|| panic!("{t}'s shim is restored by the rollback"));
            assert!(
                target.starts_with(&b18),
                "{t} points into the prior build: {}",
                target.display()
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A ROLLBACK TO AN AFFECTED TRUST BUILD LAYS ITS EXEC ROOT BEFORE ITS SHIMS. Live on
    /// trust 8595 (routed through its root), rolled back to a retained 8590 that has no root
    /// yet — both shipping `bin/rustc` as a separate file from `bin/trustc`: the undo lays
    /// `compat/trust/8590` itself and the re-pointed `tippy` shim routes through it on the
    /// first render, with no reconcile. It had come back plain, because this lay renders
    /// through `install_shim_env`, which never ensured a root (a reviewer's probe,
    /// 2026-09-15), and the single-program update and install lanes run no reconcile after
    /// a failed flip.
    #[cfg(unix)]
    #[test]
    fn rollback_to_an_affected_trust_build_lays_its_root_and_routes() {
        let dir = scratch("rb-exec-root");
        let l = layout(&dir);
        let affected = |n: u64| {
            let d = bare_build(&l, "trust", n, &["trustc", "targo", "tippy"]);
            std::fs::write(
                d.join("bin").join("trustc"),
                format!("{n} signed as trustc"),
            )
            .unwrap();
            std::fs::write(d.join("bin").join("rustc"), format!("{n} signed as rustc_")).unwrap();
            d
        };
        let b8590 = affected(8590);
        let b8595 = affected(8595);
        let exposes = vec![tool("tippy"), tool("targo")];
        activate_channel(&l, "stable", &b8595).unwrap();
        install_tools(&l, &b8595, &exposes, Aliases::Off).unwrap();
        let tippy = l.shim(&tool("tippy"));
        assert!(
            crate::compat::route_for_shim(&tippy, &b8595.join("bin").join("tippy")).is_some(),
            "fixture: 8595 routes"
        );
        assert!(std::fs::symlink_metadata(crate::compat::root_dir(&l, 8590)).is_err());

        let staged = Staged {
            build: 8595,
            build_dir: b8595,
            exposes,
            prior_build: Some(8590),
            was_live: true,
            reloc: None,
            aliases: Aliases::Off,
            tree_root: String::new(),
        };
        rollback_member(&l, "stable", "trust", &staged);

        let target = b8590.join("bin").join("tippy");
        assert_eq!(crate::platform::resolve_shim(&tippy), Some(target.clone()));
        let routed = crate::compat::root_dir(&l, 8590).join("bin").join("tippy");
        assert_eq!(
            crate::compat::route_for_shim(&tippy, &target),
            Some(routed.clone())
        );
        assert_eq!(
            std::fs::read_to_string(&tippy).unwrap(),
            crate::platform::sh_shim_content_routed(
                &target,
                &crate::shim_env::ShimEnv::NONE,
                Some(&routed)
            )
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ALIASES RIDE WITH THEIR PRIMARY through the transaction's other exits: a rollback
    /// re-points `alab-<tool>` at the prior build beside `<tool>` (and drops the alias of
    /// a tool the prior build lacks), a fresh-install rollback removes it, and a
    /// tombstone disables it — a revoked build must not stay runnable under its other
    /// name.
    #[test]
    fn aliases_are_rolled_back_and_tombstoned_with_their_primary() {
        let dir = scratch("rb-alias");
        let l = layout(&dir);
        let b18 = bare_build(&l, "ay", 18, &["ay"]);
        let b19 = bare_build(&l, "ay", 19, &["ay", "aynew"]);
        activate_channel(&l, "stable", &b18).unwrap();
        install_tools(&l, &b18, &[tool("ay")], Aliases::Alab).unwrap();
        activate_channel(&l, "stable", &b19).unwrap();
        install_tools(&l, &b19, &[tool("ay"), tool("aynew")], Aliases::Alab).unwrap();
        for t in ["ay", "aynew", "alab-ay", "alab-aynew"] {
            assert!(
                crate::platform::resolve_shim(&l.shim(&tool(t)))
                    .is_some_and(|p| p.starts_with(&b19)),
                "fixture: {t} points into 19"
            );
        }
        let staged = Staged {
            build: 19,
            build_dir: b19.clone(),
            exposes: vec![tool("ay"), tool("aynew")],
            prior_build: Some(18),
            was_live: false,
            reloc: None,
            tree_root: String::new(),
            aliases: Aliases::Alab,
        };
        rollback_member(&l, "stable", "ay", &staged);
        for t in ["ay", "alab-ay"] {
            let target = crate::platform::resolve_shim(&l.shim(&tool(t)))
                .unwrap_or_else(|| panic!("{t} is restored by the rollback"));
            assert!(target.starts_with(&b18), "{t} points into the prior build");
        }
        assert_eq!(
            crate::platform::resolve_shim(&l.shim(&tool("alab-ay"))),
            crate::platform::resolve_shim(&l.shim(&tool("ay"))),
            "the alias and its primary agree"
        );
        for t in ["aynew", "alab-aynew"] {
            assert!(
                std::fs::symlink_metadata(l.shim(&tool(t))).is_err(),
                "{t}: a tool the prior build lacks loses its shim AND its alias"
            );
        }
        // The rollback VERB reads the policy off the disk: aliases laid ⇒ kept.
        assert_eq!(Aliases::laid_for(&l, "ay"), Aliases::Alab);

        // A fresh install's rollback removes the alias with the primary.
        let fresh = Staged {
            build: 19,
            build_dir: b19,
            exposes: vec![tool("ay")],
            prior_build: None,
            was_live: false,
            reloc: None,
            tree_root: String::new(),
            aliases: Aliases::Alab,
        };
        rollback_member(&l, "stable", "ay", &fresh);
        assert!(std::fs::symlink_metadata(l.shim(&tool("ay"))).is_err());
        assert!(std::fs::symlink_metadata(l.shim(&tool("alab-ay"))).is_err());

        // Tombstoning a revoked build disables the alias too.
        install_tools(&l, &b18, &[tool("ay")], Aliases::Alab).unwrap();
        install_tombstone_shims(&l, "ay", Some(18));
        for t in ["ay", "alab-ay"] {
            let shim = l.shim(&tool(t));
            assert!(
                std::fs::symlink_metadata(&shim).is_ok_and(|m| m.file_type().is_file()),
                "{t} is a tombstone file"
            );
            assert!(
                crate::platform::resolve_shim(&shim).is_none(),
                "{t} no longer forwards anywhere"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE FRONT-OF-PATH TWIN FOLLOWS THE ROLLBACK. An agent program is laid twice — at
    /// `bin/<tool>` and again under `agents/`, which the shell hook MOVES TO THE FRONT of
    /// PATH — and the twin names its store build directly, so it only ever changes when a
    /// reconcile runs. This lane lays shims through `install_shim_env`, not
    /// `install_tools_env`, so none did: `aterm pkg rollback claude` re-pointed
    /// `bin/claude` at the prior build and said so while `agents/claude` — the copy that
    /// actually runs — kept exec'ing the build the user rolled away from, until gc
    /// reclaimed it (its claim union reads `bin/` and the `current` links, never
    /// `agents/`) and the front-of-PATH name exec'd a deleted file. The fresh-install arm
    /// is the same wound: the flip's own lay wrote the twin, and the abort is about to
    /// delete the build it names.
    #[test]
    fn rollback_repoints_the_front_of_path_agents_twin() {
        let dir = scratch("rb-agents-twin");
        let l = layout(&dir);
        let claude = tool("claude");
        let b1 = bare_build(&l, "claude", 2_026_091_001, &["claude"]);
        let b2 = bare_build(&l, "claude", 2_026_091_002, &["claude"]);
        activate_channel(&l, "stable", &b1).unwrap();
        install_tools(&l, &b1, std::slice::from_ref(&claude), Aliases::Off).unwrap();
        activate_channel(&l, "stable", &b2).unwrap();
        install_tools(&l, &b2, std::slice::from_ref(&claude), Aliases::Off).unwrap();
        let twin = l.agent_shim(&claude);
        assert_eq!(
            crate::platform::resolve_shim(&twin),
            Some(tool_bin(&b2, "claude")),
            "fixture: the twin names the build that is live"
        );

        let staged = Staged {
            build: 2_026_091_002,
            build_dir: b2.clone(),
            exposes: vec![claude.clone()],
            prior_build: Some(2_026_091_001),
            was_live: true,
            reloc: None,
            tree_root: String::new(),
            aliases: Aliases::Off,
        };
        rollback_member(&l, "stable", "claude", &staged);

        assert_eq!(
            crate::platform::resolve_shim(&l.shim(&claude)),
            Some(tool_bin(&b1, "claude")),
            "fixture: bin/claude is back on the prior build"
        );
        assert_eq!(
            crate::platform::resolve_shim(&twin),
            Some(tool_bin(&b1, "claude")),
            "the front-of-PATH twin followed it, so `claude` runs what was rolled back to"
        );

        // The fresh-install arm: no prior build to re-point, so the primary goes — and
        // nothing in `bin/` is left to vouch for the twin.
        let fresh = Staged {
            build: 2_026_091_001,
            build_dir: b1,
            exposes: vec![claude.clone()],
            prior_build: None,
            was_live: false,
            reloc: None,
            tree_root: String::new(),
            aliases: Aliases::Off,
        };
        rollback_member(&l, "stable", "claude", &fresh);
        assert!(
            std::fs::symlink_metadata(l.shim(&claude)).is_err(),
            "fixture: the fresh-install undo removed the primary"
        );
        assert!(
            std::fs::symlink_metadata(&twin).is_err(),
            "the twin goes with it rather than exec'ing a build the abort deletes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A FAILED FLIP RESTORES THE WITNESS. `activate_channel` writes the per-program link
    /// before the channel link; when the channel half fails, `flip_member` points the
    /// witness back at the prior build — the shims were never touched, so witness and
    /// shims keep agreeing — instead of leaving it naming the build the abort cleanup
    /// deletes (a broken own link makes GC abstain on the program until the next
    /// successful activation).
    #[test]
    fn a_failed_flip_points_the_witness_back_at_the_prior_build() {
        let dir = scratch("flip-witness");
        let l = layout(&dir);
        let b18 = bare_build(&l, "ay", 18, &["ay"]);
        let b19 = bare_build(&l, "ay", 19, &["ay"]);
        activate_channel(&l, "stable", &b18).unwrap();
        install_tools(&l, &b18, &[tool("ay")], Aliases::Off).unwrap();
        // Make the CHANNEL half fail strictly after the witness half: a regular FILE where
        // the channel DIRECTORY must go, so `ensure_private_dir(channels/beta)` errs.
        std::fs::create_dir_all(l.prefix.join("channels")).unwrap();
        std::fs::write(l.prefix.join("channels").join("beta"), b"not a dir").unwrap();

        let staged = Staged {
            build: 19,
            build_dir: b19,
            exposes: vec![tool("ay")],
            prior_build: Some(18),
            was_live: false,
            reloc: None,
            aliases: Aliases::Off,
            tree_root: String::new(),
        };
        assert!(
            !flip_member(&l, "beta", "ay", &staged),
            "the flip must report failure"
        );
        assert_eq!(
            std::fs::read_link(l.program_current("ay")).expect("witness link survives"),
            b18,
            "the witness points back at the prior build, agreeing with the untouched shims"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The fresh-install variant of the failed flip: no prior build exists, so the repair
    /// REMOVES the witness link the flip wrote — the abort cleanup deletes the build it
    /// named, and a program that was never installed must not keep a (dangling) witness.
    #[test]
    fn a_failed_fresh_install_flip_removes_the_witness_link() {
        let dir = scratch("flip-fresh");
        let l = layout(&dir);
        let b19 = bare_build(&l, "ay", 19, &["ay"]);
        std::fs::create_dir_all(l.prefix.join("channels")).unwrap();
        std::fs::write(l.prefix.join("channels").join("beta"), b"not a dir").unwrap();

        let staged = Staged {
            build: 19,
            build_dir: b19,
            exposes: vec![tool("ay")],
            prior_build: None,
            was_live: false,
            reloc: None,
            aliases: Aliases::Off,
            tree_root: String::new(),
        };
        assert!(!flip_member(&l, "beta", "ay", &staged));
        assert!(
            std::fs::symlink_metadata(l.program_current("ay")).is_err(),
            "no witness link survives a failed fresh install"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------------------------------------------------------------------------
    // THE CLOCK. A clock we cannot read must refuse everything, never admit everything.
    // ---------------------------------------------------------------------------------

    /// An unreadable clock yields `i64::MAX`, not `0`.
    ///
    /// This value drives the ROSTER's `valid_until` and every machine's `not_after`, and
    /// zero reads as 1970 — before every conceivable deadline — so it would ADMIT a roster
    /// generation that lapsed years ago and treat an expired machine as live. Roster
    /// freshness is the only defence a fresh install has (it carries no floor), so this is
    /// the one direction that must not invert.
    ///
    /// The error is a REAL `SystemTimeError`, not a stand-in: `UNIX_EPOCH.duration_since(now)`
    /// fails on any post-1970 clock, which the first assertion states as a precondition so
    /// the test cannot pass by never reaching the fallback.
    #[test]
    fn an_unreadable_clock_fails_closed_rather_than_reading_as_1970() {
        let err = std::time::UNIX_EPOCH.duration_since(std::time::SystemTime::now());
        assert!(
            err.is_err(),
            "precondition: this machine's clock is after 1970, so we really do have a \
             SystemTimeError to feed the fallback"
        );
        assert_eq!(
            unix_or_fail_closed(err),
            i64::MAX,
            "an unreadable clock must make every window look EXPIRED"
        );
        // NON-VACUITY: the readable path is unaffected and still returns the real second.
        let ok = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
        assert!(unix_or_fail_closed(ok) > 1_700_000_000);
    }

    /// ...and the direction MATTERS, proved on the gate itself rather than on the constant:
    /// the same lapsed roster is REFUSED under the fail-closed sentinel and ADMITTED under
    /// the old `0`.
    ///
    /// MUTATION: put `.unwrap_or(0)` back in `unix_or_fail_closed` and the first assertion
    /// here still passes (it uses the literal), but `now_unix`'s callers start behaving like
    /// the second — which is why this test asserts BOTH readings of the same bytes.
    #[test]
    fn the_clock_sentinel_is_what_refuses_a_lapsed_roster() {
        let mut r = testkit::roster();
        r.valid_until = "2020-01-01T00:00:00Z".into();
        let bytes = r.to_toml().expect("a valid roster emits").into_bytes();
        let sig = testkit::sign(&testkit::MASTER_SEED, &bytes);
        let anchor = anchor();

        assert_eq!(
            crate::sig::admit_roster(&anchor, bytes.clone(), &sig, i64::MAX).err(),
            Some(crate::sig::Reject::Stale),
            "the fail-closed sentinel refuses a roster whose window lapsed"
        );
        assert!(
            crate::sig::admit_roster(&anchor, bytes.clone(), &sig, 0).is_ok(),
            "precondition: 0 really would have admitted it — that is the bug this guards"
        );
        // NON-VACUITY: the identical pair is admitted inside its window, so the refusal
        // above is the clock and not the fixture.
        let mut live = testkit::roster();
        live.valid_until = "2099-01-01T00:00:00Z".into();
        let live_bytes = live.to_toml().unwrap().into_bytes();
        let live_sig = testkit::sign(&testkit::MASTER_SEED, &live_bytes);
        assert!(crate::sig::admit_roster(&anchor, live_bytes, &live_sig, testkit::NOW).is_ok());
    }

    /// The strandage bound a resumable download needs: OUR partial survives (the next
    /// attempt continues it), every other program-staging partial is reclaimed, and
    /// nothing else in the directory is touched.
    ///
    /// Without the sweep a resumable lane trades "re-fetch 630 MB on every retry" for
    /// "leak a 600 MB prefix per abandoned build", which is not a trade worth making.
    #[test]
    fn a_resumable_download_strands_at_most_one_partial_per_program() {
        let dir = std::env::temp_dir().join(format!(
            "atpkg-sweep-part-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dl = dir.join("trust-5520.tar.zst");
        let ours = dir.join("trust-5520.tar.zst.part");
        let stale = dir.join("trust-5100.tar.zst.part");
        let bystander = dir.join("notes.txt");
        let finished = dir.join("trust-5100.tar.zst");
        for p in [&ours, &stale, &bystander, &finished] {
            std::fs::write(p, b"x").unwrap();
        }

        sweep_foreign_partials(&dl);

        assert!(
            ours.exists(),
            "OUR partial is what the next attempt resumes"
        );
        assert!(!stale.exists(), "a superseded build's partial is reclaimed");
        assert!(
            bystander.exists(),
            "a file that is neither partial nor archive is left alone"
        );
        // Since archives of a failed stage are RETAINED for reuse (2026-09-15), a complete
        // archive of ANOTHER asset name is swept too: at most one of each survives per
        // program, and it is this pass's.
        assert!(
            !finished.exists(),
            "a superseded build's retained archive is reclaimed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The GROUP path holds the same strandage bound as the singleton path: staging a
    /// member reclaims every foreign partial in that program's staging dir (the mirror
    /// of [`a_resumable_download_strands_at_most_one_partial_per_program`], driven
    /// through `apply_channel` so it pins `stage_member`'s call, not just the helper).
    #[test]
    fn a_group_member_stage_strands_at_most_one_partial_per_program() {
        let dir = scratch("group-part-sweep");
        let fake = group_fixture(&dir);
        let layout = layout(&dir);
        let staging = layout.staging_dir("trust");
        std::fs::create_dir_all(&staging).unwrap();
        let stale = staging.join("trust-4700.tar.zst.part");
        let bystander = staging.join("notes.txt");
        for p in [&stale, &bystander] {
            std::fs::write(p, b"x").unwrap();
        }
        let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
        let report = apply_channel(
            &fake,
            &layout,
            &anchor(),
            "stable",
            TRIPLE,
            &installed,
            &[],
            fl(0),
            0,
        )
        .unwrap();
        assert!(
            layout.build_dir("trust", 4821).exists(),
            "reach guard: the group member actually staged — report {report:?}"
        );
        assert!(
            !stale.exists(),
            "a superseded build's partial is reclaimed on the group path too"
        );
        assert!(bystander.exists(), "only `.part` files are swept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The digest gate's belt-and-braces arm: an asset that FAILS its signed sha256 also
    /// takes any sibling `.part` with it, so a poisoned prefix can never seed (and
    /// re-fail) the next attempt. The next attempt refetches whole — and WHEN there is a
    /// next attempt is the digest-refusal cooldown's business ([`digest_refusal_note`]),
    /// which is why that refetch is no longer once every six hours forever.
    #[test]
    fn a_sha256_mismatch_discards_the_sibling_partial() {
        let dir = scratch("sha-part");
        let fake = fixture(&dir);
        let layout = layout(&dir);
        // Corrupt the SOURCE after the manifest signed the honest sha256, so the
        // downloaded bytes can never match the signed value.
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(dir.join("ay-18.tar.zst"))
                .unwrap();
            f.write_all(b"poison").unwrap();
        }
        let staging = layout.staging_dir("ay");
        std::fs::create_dir_all(&staging).unwrap();
        let part = staging.join("ay-18.tar.zst.part");
        std::fs::write(&part, b"poisoned prefix").unwrap();
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let err = install(&fake, &layout, &anchor(), &req, fl(0), 0)
            .expect_err("a corrupted asset must fail the signed digest gate");
        assert!(
            matches!(&err, FlowError::Stage(StageError::Sha256Mismatch { .. })),
            "reach guard: the failure is the digest gate, not something earlier — {err:?}"
        );
        assert!(
            !part.exists(),
            "the sibling partial goes with the digest-failing asset"
        );
        assert!(
            !staging.join("ay-18.tar.zst").exists(),
            "the failed asset itself is reclaimed, as before"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A [`Fake`] that COUNTS the artifact downloads it is asked for, by asset name.
    /// Transfers are the whole subject of the two tests below, and the only honest way to
    /// measure "this pass moved no bytes" is to ask the fetcher.
    struct Counting {
        inner: Fake,
        downloads: std::cell::RefCell<HashMap<String, usize>>,
    }
    impl Counting {
        fn of(inner: Fake) -> Self {
            Self {
                inner,
                downloads: std::cell::RefCell::new(HashMap::new()),
            }
        }
        fn count(&self, asset: &str) -> usize {
            self.downloads.borrow().get(asset).copied().unwrap_or(0)
        }
    }
    impl Fetcher for Counting {
        fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
            self.inner.index_candidates()
        }
        fn pkg_manifest(
            &self,
            repo: &str,
            program: &str,
            build: u64,
        ) -> Result<(Vec<u8>, Vec<u8>), String> {
            self.inner.pkg_manifest(repo, program, build)
        }
        fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
            *self
                .downloads
                .borrow_mut()
                .entry(asset.to_string())
                .or_default() += 1;
            self.inner.download(repo, asset, dest)
        }
    }

    /// Append bytes to a fixture's archive AFTER its manifest signed the honest sha256, so
    /// every download of it fails the signed digest gate — a publish-pipeline slip, exactly
    /// as a machine meets it.
    fn poison(archive: &Path) {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(archive)
            .unwrap();
        f.write_all(b"poison").unwrap();
    }

    /// Rewrite a refusal memo's `at=` line so its cooldown has lapsed, the way the clock
    /// does on a real machine.
    fn age_refusal(memo: &Path) {
        let text = std::fs::read_to_string(memo).unwrap();
        let aged: Vec<String> = text
            .lines()
            .map(|l| {
                if l.starts_with("at=") {
                    String::from("at=0")
                } else {
                    l.to_string()
                }
            })
            .collect();
        std::fs::write(memo, aged.join("\n") + "\n").unwrap();
    }

    /// THE REFETCH BOUND, singleton lane. Re-proving that a published asset does not match
    /// its pin's signed sha256 costs the whole asset, every six hours, for as long as the
    /// pin stands. The first retry is still free — one mismatch can be a truncated
    /// transfer, and a repair under the same pin must be found — but the SECOND identical
    /// verdict arms a cooldown that holds the next pass off the wire entirely and SAYS
    /// why; once it lapses the pass tries again, once, and the memo re-arms for longer.
    #[test]
    fn a_digest_refusal_is_not_refetched_on_every_tick() {
        let dir = scratch("digest-refusal");
        let fake = Counting::of(fixture(&dir));
        let layout = layout(&dir);
        poison(&dir.join("ay-18.tar.zst"));
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let first = install(&fake, &layout, &anchor(), &req, fl(0), 0)
            .expect_err("a corrupted asset must fail the signed digest gate");
        assert!(
            matches!(&first, FlowError::Stage(StageError::Sha256Mismatch { .. })),
            "reach guard: the first attempt reached the digest gate — {first:?}"
        );
        assert_eq!(fake.count("ay-18.tar.zst"), 1, "it had to download it once");
        let memo = layout.build_dir("ay", 18).with_file_name("18.refused");
        assert!(memo.is_file(), "the verdict is recorded beside the build");

        // THE FIRST RETRY IS FREE — one mismatch can be a truncated transfer, and a
        // publisher who repairs the asset under the same pin must be found on the next
        // tick (`a_member_stage_failure_aborts_the_group_and_a_retry_heals_it` pins
        // exactly that healing). So this pass downloads again, and it is its IDENTICAL
        // verdict that arms the cooldown.
        let second = install(&fake, &layout, &anchor(), &req, fl(0), 0)
            .expect_err("a corrupted asset must fail the signed digest gate");
        assert!(
            matches!(&second, FlowError::Stage(StageError::Sha256Mismatch { .. })),
            "the first retry is never refused — {second:?}"
        );
        assert_eq!(fake.count("ay-18.tar.zst"), 2, "it really did retry");

        // The tick after THAT, pin unchanged: no transfer at all, and the refusal carries
        // the original stage failure's own words plus the way out.
        let third = install(&fake, &layout, &anchor(), &req, fl(0), 0)
            .expect_err("the pin still names digests the asset does not match");
        let FlowError::StageRefused(note) = &third else {
            panic!("the third tick must be refused by the memo, not re-staged: {third:?}");
        };
        assert!(
            note.contains("asset sha256 mismatch") && note.contains("aterm pkg install ay"),
            "the refusal names the original verdict and the way out: {note}"
        );
        assert_eq!(
            fake.count("ay-18.tar.zst"),
            2,
            "every six-hourly tick re-downloaded the whole asset; it must not"
        );

        // Once the cooldown lapses the machine asks again — exactly once — and the memo
        // re-arms, longer, so a publisher's repair under the same pin is never hidden
        // forever.
        age_refusal(&memo);
        let fourth = install(&fake, &layout, &anchor(), &req, fl(0), 0)
            .expect_err("the lapsed cooldown lets the attempt through");
        assert!(
            matches!(&fourth, FlowError::Stage(StageError::Sha256Mismatch { .. })),
            "a lapsed memo refuses nothing — {fourth:?}"
        );
        assert_eq!(
            fake.count("ay-18.tar.zst"),
            3,
            "one attempt, not four a day"
        );
        assert!(
            std::fs::read_to_string(&memo)
                .unwrap()
                .contains("attempts=3"),
            "the refusal re-armed, longer"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE REFETCH BOUND, group lane — the one the fleet meets, because the biggest
    /// member of the `rustc` tuple is the multi-gigabyte one. The tuple still aborts (a
    /// member that cannot stage aborts the group, as always) and still retries once for
    /// free; what stops is paying for the download to be told the same thing a third
    /// time, and the abort's own sentence says so.
    #[test]
    fn a_group_member_s_digest_refusal_is_not_refetched_on_every_tick() {
        let dir = scratch("group-digest-refusal");
        let fake = Counting::of(group_fixture(&dir));
        let layout = layout(&dir);
        poison(&dir.join("trust-4821.tar.zst"));
        let installed = std::collections::BTreeMap::from([("ay".to_string(), 17u64)]);
        let apply = |f: &Counting| {
            apply_channel(
                f,
                &layout,
                &anchor(),
                "stable",
                TRIPLE,
                &installed,
                &[],
                fl(0),
                0,
            )
            .unwrap()
        };

        let first = apply(&fake);
        let (_, outcome) = &first.groups[0];
        assert!(
            matches!(outcome, TxnOutcome::Aborted { failed, .. } if failed == "trust"),
            "reach guard: the tuple aborted on the poisoned member — {outcome:?}"
        );
        assert_eq!(fake.count("trust-4821.tar.zst"), 1);
        assert!(
            layout
                .build_dir("trust", 4821)
                .with_file_name("4821.refused")
                .is_file(),
            "the member's verdict is recorded beside its build"
        );

        // The free first retry: this pass really does fetch the bundle again.
        let second = apply(&fake);
        let (_, outcome) = &second.groups[0];
        assert!(
            matches!(outcome, TxnOutcome::Aborted { failed, .. } if failed == "trust"),
            "the first retry is never refused — {outcome:?}"
        );
        assert_eq!(fake.count("trust-4821.tar.zst"), 2, "it really did retry");

        // The tick after that: the same abort, having moved no bytes for the member.
        let third = apply(&fake);
        let (_, outcome) = &third.groups[0];
        let TxnOutcome::Aborted { failed, why, .. } = outcome else {
            panic!("the tuple still aborts on the member it cannot stage: {outcome:?}");
        };
        assert_eq!(failed, "trust");
        assert!(
            why.contains("not refetching"),
            "the abort says the transfer was skipped and why: {why}"
        );
        assert_eq!(
            fake.count("trust-4821.tar.zst"),
            2,
            "every six-hourly tick re-downloaded the whole bundle; it must not"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
