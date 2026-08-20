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
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::activate::{activate_channel, install_tombstone_shim, install_tools};
use crate::apply::{Group, TxnOutcome, plan_groups, transact};
use crate::gate::{ApplyDecision, decide};
use crate::install::{StageError, verify_and_stage};
use crate::manifest::{Channel, Index, parse_pkg};
use crate::select::{Candidate, select_index};
use crate::sig::{Anchor, BuildFloor, TrustedIndex};
use crate::store::{Layout, ToolName};

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
    /// [`Fetcher::download`], additionally told WHICH program the asset belongs to.
    /// Default: identical to `download`. The production GitHub fetcher overrides
    /// this to honor a per-program `[packages.links]` `owner/repo` FETCH override
    /// (a possibly-private repo the program's release assets are pulled from).
    /// The host is never an authenticity input (§5/§8): the bytes still pass the
    /// identical signed-manifest sha256 + `tree_root` gates, so a redirected
    /// fetch can only supply bytes, not trust. The flow calls THIS for artifact
    /// downloads so the override reaches both the install and the stage paths.
    fn download_for(
        &self,
        program: &str,
        repo: &str,
        asset: &str,
        dest: &Path,
    ) -> Result<(), String> {
        let _ = program;
        self.download(repo, asset, dest)
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
    /// Skipped, with a reason (unreachable / tombstoned / not pinned / cycle / fetch error).
    Skipped(String),
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
    /// The program is not named in the verified index (unreachable, §5).
    NotReachable(String),
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
    /// The signed `program`/`build_number` did not match the request (anti-replay).
    Mismatch,
    /// No artifact for the target triple (a clean skip, §6).
    NoArtifact(String),
    /// The artifact's `kind` is not installable by this tool path — an unrecognized kind.
    /// Fail-closed (§16.4 dispatch).
    UnsupportedKind(String),
    /// An `app-bundle` member was refused by the two-anchor app-apply gate
    /// ([`crate::appgate::app_apply_allowed`], §16.2/§16.4). The notarized DMG self-swap is a
    /// distinct topology the CLI tool-install path does not carry, so the gate's unconditional
    /// notarization AND-anchor is unproven here and the decision fails closed.
    AppBundleRefused(String),
    /// The asset download failed.
    Download(String),
    /// Staging (sha256 / extract / tree_root) failed.
    Stage(StageError),
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
                f.write_str(") — this is a network problem, not a signature problem; \
                             the toolchain retries automatically")
            }
            FlowError::NotReachable(p) => {
                f.write_str(p)?;
                f.write_str(" is not named in the signed index")
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
                f.write_str("'s app-bundle was refused by the app-apply gate (notarized self-swap \
                             not wired on the CLI install path — the notarization anchor is unproven)")
            }
            FlowError::Download(e) => {
                f.write_str("download: ")?;
                f.write_str(e)
            }
            FlowError::Stage(e) => {
                f.write_str("stage: ")?;
                std::fmt::Display::fmt(e, f)
            }
            FlowError::Activate(e) => {
                f.write_str("activate: ")?;
                f.write_str(e)
            }
            FlowError::Rollback(m) => {
                f.write_str("rollback: ")?;
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
                f.write_str(" is dev-linked; run `atpkg unlink ")?;
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
    let mut seen = BTreeSet::new();
    install_inner(
        fetcher,
        layout,
        anchor,
        req,
        floor,
        now_unix,
        &mut seen,
    )
}

/// [`install`] with a cycle-guard `seen` set threaded through the `requires` recursion (§17):
/// a dependency that requires back into a program already on the resolution stack is skipped
/// rather than looping.
#[allow(
    clippy::too_many_arguments,
    reason = "install_inner is install plus the recursion cycle-guard set; every other input \
              is an irreducible dependency of a verified install (fetcher, layout, root key, \
              the request, and the floor + clock the anti-rollback/freshness gates read)"
)]
fn install_inner(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    req: &InstallRequest,
    floor: BuildFloor,
    now_unix: i64,
    seen: &mut BTreeSet<String>,
) -> Result<InstallReport, FlowError> {
    let (channel, program, triple, installed) =
        (req.channel, req.program, req.triple, req.installed);
    // 0. Dev-link HARD-SKIP (§13): a linked program is managed from its checkout, not the
    //    registry — short-circuit before any network I/O so link composes ahead of every
    //    other gate.
    if crate::linkmode::is_linked(layout, program) {
        return Err(FlowError::Linked(program.to_string()));
    }
    seen.insert(program.to_string());
    // 1–2. Resolve + verify-select the index + freshness (§8 gate 2) — the shared
    // [`resolve_verified_index`] prologue (cached-fallback, §14) — then reachability.
    let index = resolve_verified_index(fetcher, layout, anchor, floor, now_unix)?;
    let repo = index
        .program(program)
        .ok_or_else(|| FlowError::NotReachable(program.to_string()))?
        .repo
        .clone();
    let ch = index
        .channels
        .iter()
        .find(|c| c.name == channel)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    let &pinned = ch
        .pin
        .get(program)
        .ok_or_else(|| FlowError::NotPinned(program.to_string()))?;

    // 3. The apply decision.
    match decide(ch, program, installed) {
        ApplyDecision::UpToDate => {
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
    let verified = index.verify_pkg(raw, &sig).map_err(|_| FlowError::PkgVerify)?;
    let pkg = parse_pkg(&verified).map_err(|_| FlowError::PkgParse)?;
    if !pkg.is_for(program) || pkg.build_number != pinned {
        return Err(FlowError::Mismatch);
    }

    // 4b. Runtime `requires` (§17): pull in each MISSING dep FIRST, through the SAME verified
    // pipeline (reachability + freshness + floor/yank gate). Best-effort — a dep failure never
    // fails the parent install; a Tombstone/NotReachable is SKIPPED, never bypassed. `requires`
    // is SIGNED metadata (parsed from the just-verified &VerifiedBytes), so a repo-write
    // adversary can neither inject nor redirect a dependency edge.
    let mut dependencies: Vec<DepOutcome> = Vec::new();
    // ONE `bin/` scan for the whole loop: any program this recursion installs is in
    // `seen` (inserted at entry) and screened by the check below BEFORE the map is
    // consulted, so a pre-loop snapshot decides identically.
    let active = crate::ops::active_builds(layout);
    for dep in &pkg.requires {
        if dep.as_str() == program || seen.contains(dep) {
            dependencies.push(DepOutcome {
                program: dep.clone(),
                result: DepResult::Skipped("already resolved or cycle".into()),
            });
            continue;
        }
        if let Some(b) = active.get(dep).copied() {
            dependencies.push(DepOutcome {
                program: dep.clone(),
                result: DepResult::AlreadyPresent(b),
            });
            continue;
        }
        let dep_req = InstallRequest {
            channel,
            program: dep.as_str(),
            triple,
            installed: None,
        };
        match install_inner(
            fetcher,
            layout,
            anchor,
            &dep_req,
            floor,
            now_unix,
            seen,
        ) {
            Ok(r) => {
                dependencies.push(DepOutcome {
                    program: dep.clone(),
                    result: DepResult::Installed {
                        build: r.build,
                        tree_root: r.tree_root.clone(),
                    },
                });
                dependencies.extend(r.dependencies); // flatten the transitive closure
            }
            Err(e) => dependencies.push(DepOutcome {
                program: dep.clone(),
                result: DepResult::Skipped(e.to_string()),
            }),
        }
    }

    // 5. The artifact for this triple (missing triple = clean fail-closed skip).
    let artifact = pkg
        .artifact_for(triple)
        .ok_or_else(|| FlowError::NoArtifact(triple.to_string()))?;
    // Per-member dispatch (§16.4): the tool path installs plain `binary`/`cargo-src`
    // (Shim) AND now `sysroot-bundle` (trust / trust-mc) artifacts. A sysroot-bundle
    // gets bundle-specific wiring ([`apply_sysroot_bundle`]) BEFORE activation plus a
    // fail-loud resolve check AFTER — a failure past activation UNWINDS
    // ([`abort_activated_install`]) so a broken toolchain is neither reported SUCCESS
    // nor left live reading as 'already current'. `app-bundle` (notarized self-swap)
    // and unknown kinds remain refused CLOSED. (audit: sysroot-bundle
    // silent-broken-install; sysroot-bundle resolve-failure left-active wedge.)
    let strategy = crate::dispatch::strategy_for(&artifact.kind);
    match strategy {
        crate::dispatch::ApplyStrategy::Shim | crate::dispatch::ApplyStrategy::SysrootBundle => {}
        crate::dispatch::ApplyStrategy::AppBundle => {
            // Drive the two-anchor app-apply gate for the notarized self-swap topology and
            // fail closed (the DMG self-swap itself is a documented TODO, see the helper).
            return Err(app_apply_gate_refused(
                ch, program, pinned, artifact, installed,
            ));
        }
        crate::dispatch::ApplyStrategy::Unknown => {
            return Err(FlowError::UnsupportedKind(artifact.kind.clone()));
        }
    }

    // 6. Download → verify-and-stage (sha256 → extract → tree_root re-verify).
    let dl = layout.staging_dir(program).join(&artifact.asset);
    // Disk preflight (§9): the compressed asset + its extracted tree must fit (they coexist
    // until the asset is reclaimed post-stage) while keeping the free floor. Fails OPEN when
    // free space can't be queried (available_bytes None) — preflight is a safety net.
    let required = artifact.size.saturating_add(artifact.cost.disk_installed);
    disk_gate(required, crate::freespace::available_bytes(&dl))?;
    if let Some(parent) = dl.parent() {
        std::fs::create_dir_all(parent).map_err(|e| FlowError::Download(e.to_string()))?;
    }
    // Clear any stale staging file BEFORE fetching. A leftover from a killed run is
    // not merely junk: for a `dir:` registry it is a HARDLINK to the source, and
    // writing "into" it writes into the registry — `curl -o` truncates it, and
    // `fs::copy` truncates the shared inode. Both destroy a file inside the user's
    // signed app bundle. See `DirFetcher::download`.
    let _ = std::fs::remove_file(&dl);
    if let Err(e) = fetcher.download_for(program, &repo, &artifact.asset, &dl) {
        // An aborted transfer leaves BYTES here: the production fetcher hands curl `-o <dl>`
        // with no `--remove-on-error` and no `.part`+rename, so a timed-out multi-GB body is
        // still on disk. It is never resumed (the retry re-fetches from zero), so it is pure
        // strandage — reclaim it on this exit exactly as on the stage exit below.
        let _ = std::fs::remove_file(&dl);
        return Err(FlowError::Download(e));
    }
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
    let staged = verify_and_stage(artifact, &dl, &build_dir);
    let _ = std::fs::remove_file(&dl);
    staged.map_err(FlowError::Stage)?;

    // 6b. Sysroot-bundle wiring BEFORE activation (self-contained = no-op).
    if strategy == crate::dispatch::ApplyStrategy::SysrootBundle {
        apply_sysroot_bundle(&artifact.reloc)?;
    }

    // 7. Activate + shim. The raw manifest `exposes` is admitted ONCE here; `tools` is what
    // actually got a shim and `refused` the sensitive/malformed names that did not.
    activate_channel(layout, channel, &build_dir)
        .map_err(|e| FlowError::Activate(e.to_string()))?;
    let (tools, refused) = crate::store::split_exposed(&pkg.exposes);
    // Past activation a failure leaves the broken build LIVE — channel `current`,
    // per-program witness, any shims already written — AND carrying its `.ready`
    // marker, so `active_builds` reports it, `decide` calls it UpToDate, and a retry
    // prints 'already current' with nothing working (the wedge `flip_member` rolls
    // back on the transactional path). Capture the same rollback input here so both
    // error arms below unwind identically. (audit: resolve-failure left-active wedge.)
    let staged = Staged {
        build: pinned,
        build_dir: build_dir.clone(),
        exposes: tools.clone(),
        prior_build: installed,
        was_live,
        reloc: None,
        tree_root: String::new(),
    };
    if let Err(e) = install_tools(layout, &build_dir, &tools) {
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
    })
}

/// Install a failing tombstone shim (§7) over EVERY tool `program`'s currently-active build
/// exposes, so a yanked/below-floor build's old working shims are actively disabled. The tool
/// set is the program's live `bin/` shims (the exact "old working shims" to revoke); if the
/// program is not installed there is nothing to disable. Best-effort per tool (a write failure
/// on one shim never blocks the rest); a tombstone can never shadow a sensitive name because
/// `active_tools` hands back [`crate::store::ToolName`]s, which only exist for admitted names.
fn install_tombstone_shims(layout: &Layout, program: &str, installed: Option<u64>) {
    if let Some(cur) = installed {
        for tool in crate::ops::active_tools(layout, program, cur) {
            let _ = install_tombstone_shim(layout, &tool);
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
/// survives, either: `undo_activation` still runs, so no shim and no `current` link resolves
/// into the tree, `active_builds` is silent about it, and the next run re-activates it from
/// disk instead of re-fetching it. Reached whenever the SHIM-derived `installed` view is
/// silent for a live program (`atpkg unlink`, tombstoned tools, dev-link mode) and `decide`
/// therefore returns Install for the build `store/<program>/current` already names — the
/// same blind spot [`Staged::was_live`] exists for in the group transaction.
fn abort_activated_install(layout: &Layout, channel: &str, program: &str, staged: &Staged) {
    rollback_member(layout, channel, program, staged);
    crate::activate::undo_activation(layout, channel, &staged.build_dir);
    if staged.was_live {
        return;
    }
    crate::store::discard_build(&staged.build_dir);
}

/// Drive the two-anchor app-apply gate ([`crate::appgate::app_apply_allowed`], §16.2/§16.4)
/// for an `app-bundle` member met on the tool-install path, and return the fail-closed refusal.
///
/// `aterm.app` is a NOTARIZED DMG applied by a self-swap (`renamex_np(RENAME_SWAP)` + re-exec),
/// a distinct topology from the `bin/` symlink flip. The atpkg CLI tool-install path does not
/// carry that swap, so Apple **notarization is unproven here** — and because notarization is
/// the gate's UNCONDITIONAL AND-anchor, the decision fails closed regardless of the index
/// conjunct. We still evaluate the REAL gate (with the fresh-index conjunct built from the
/// signed channel `min_build` + per-build yank state) so the refusal is the gate's decision,
/// not a blanket reject, and so the swap can later be wired behind a `true` notarization result.
///
/// TODO(app-bundle self-swap): once atpkg can stage the DMG and run the notarized self-swap
/// (the `aterm-update` topology), pass the real notarization result + staged-DMG sha256 here
/// and perform the swap when [`crate::appgate::app_apply_allowed`] returns true.
fn app_apply_gate_refused(
    ch: &Channel,
    program: &str,
    pinned: u64,
    _artifact: &crate::manifest::Artifact,
    installed: Option<u64>,
) -> FlowError {
    let gate = crate::appgate::AppIndexGate {
        // No staged DMG to hash on this path yet, so the sha256 conjunct cannot be satisfied
        // (part of the documented TODO); the notarization anchor already fails the gate closed.
        sha256_match: false,
        min_build: ch.min_build,
        yanked: crate::gate::is_yanked(ch, program, pinned),
    };
    // notarized = false: the CLI install path proves no Apple notarization and runs no
    // self-swap, so the unconditional AND-anchor refuses the apply.
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
    floor: BuildFloor,
    now_unix: i64,
) -> Result<ChannelApplyReport, FlowError> {
    // 1–2. Resolve + verify-select the index ONCE + freshness (§8) — the shared
    //      [`resolve_verified_index`] prologue (cached-fallback, §14).
    let index = resolve_verified_index(fetcher, layout, anchor, floor, now_unix)?;
    let ch = index
        .channels
        .iter()
        .find(|c| c.name == channel)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?
        .clone();

    // 3. Partition the channel's pins into coherence groups (grouped tuples + singletons)
    //    and apply each as its own all-or-nothing transaction.
    let groups = plan_groups(&index, &ch);
    let mut results = Vec::with_capacity(groups.len());
    let mut applied: BTreeMap<String, AppliedMember> = BTreeMap::new();
    let mut skipped_linked: Vec<String> = Vec::new();
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
            fetcher, layout, &index, &ch, channel, triple, group, installed,
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
    })
}

/// Apply ONE coherence group as an all-or-nothing transaction (the per-group body factored
/// out of [`apply_channel`], shared with the transactional [`apply_program`] update path).
/// `None` ⇒ the group has no installed member and was skipped (that would be a fresh
/// `install`, not an update). Otherwise `Some(outcome)`.
///
/// [`crate::gate::decide`] is the security AUTHORITY, evaluated FIRST. Two consumer gates run
/// strictly AFTER it and can only SUPPRESS/ABORT, never move a build:
/// * **local pin** — if any member is pinned AND the group is not tombstoning AND it wants an
///   upgrade, the whole tuple is held on its current builds ([`TxnOutcome::Pinned`]); a
///   Tombstone anywhere makes the pin IGNORED so a revoked build never keeps running;
/// * **disk preflight** — a group-aggregated shortfall aborts the group before staging so it
///   stays coherent on its current builds; fails OPEN on any query/fetch failure.
#[allow(
    clippy::too_many_arguments,
    reason = "the per-group apply needs the same irreducible inputs as apply_channel: the \
              network fetcher, the layout, the verified index + channel, the channel name + \
              triple selectors, the group, and the installed-build map"
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
) -> Option<(Group, TxnOutcome, BTreeMap<String, AppliedMember>)> {
    // `update` touches INSTALLED groups only: skip a group with no installed member (that
    // would be a fresh `install`, not an update). A coherence group with even ONE member
    // installed IS processed in full — the locked tuple must stay coherent, so a missing
    // sibling is pulled in to the pin (decide → Install).
    if group.members.iter().all(|m| !installed.contains_key(m)) {
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
    // NOTE — `[packages].exclude` is deliberately NOT read here, even though
    // `uninstall` tells the user it is the way to "keep the set and drop just this
    // one". I wired it in and took it out again: `config::cached()` is a process-global
    // OnceLock over the INVOKING user's aterm.toml, and this layer decides against a
    // caller-supplied `layout`. Reading it here made a synthetic-layout call apply some
    // other prefix's exclusions and made this file's own unit tests depend on the
    // developer's real config — in a module that refuses to touch the real `~/.aterm`
    // for exactly that reason (2026-08-20 round-13 audit).
    //
    // The two records consulted below both come from the layout, so they are honest for
    // whatever store is being acted on. Honouring `exclude` needs it threaded in the
    // same way; until then the gap is a documented one rather than hidden global state.
    let deliberately_absent =
        |m: &String| (declined || removed.contains(m)) && !installed.contains_key(m);
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
        let (outcome, applied) =
            apply_group_txn(fetcher, layout, index, ch, channel, triple, &present, installed);
        return Some((present, outcome, applied));
    }
    let (outcome, applied) =
        apply_group_txn(fetcher, layout, index, ch, channel, triple, group, installed);
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
#[allow(
    clippy::too_many_arguments,
    reason = "the bootstrap group apply needs the same irreducible inputs as apply_group \
              minus the pre-resolved channel it looks up itself: fetcher, layout, verified \
              index, channel + triple selectors, the group, and the installed-build map"
)]
pub fn bootstrap_group(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    channel: &str,
    triple: &str,
    group: &Group,
    installed: &BTreeMap<String, u64>,
) -> Result<(TxnOutcome, BTreeMap<String, AppliedMember>), FlowError> {
    let ch = index
        .channels
        .iter()
        .find(|c| c.name == channel)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    Ok(apply_group_txn(
        fetcher, layout, index, ch, channel, triple, group, installed,
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
    let ch = index.channels.iter().find(|c| c.name == channel)?;
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
/// [`bootstrap_group`] (§11 fresh install): decide-first, the local-pin hold, the group-
/// aggregated disk preflight, then stage-all → flip-all → rollback via [`transact`], with
/// the abort discard, tombstone-shim disable, and applied `tree_root` capture.
#[allow(
    clippy::too_many_arguments,
    reason = "the per-group apply needs the same irreducible inputs as apply_channel: the \
              network fetcher, the layout, the verified index + channel, the channel name + \
              triple selectors, the group, and the installed-build map"
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
) -> (TxnOutcome, BTreeMap<String, AppliedMember>) {
    let decisions: Vec<(String, ApplyDecision)> = group
        .members
        .iter()
        .map(|m| (m.clone(), decide(ch, m, installed.get(m).copied())))
        .collect();

    // LOCAL PIN GATE — strictly AFTER decide(), suppression-only, coherence-preserving.
    let any_tombstone = decisions
        .iter()
        .any(|(_, d)| *d == ApplyDecision::Tombstone);
    let wants_upgrade = decisions.iter().any(|(_, d)| *d == ApplyDecision::Install);
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
        // every installed member's current build is itself still gate-valid. If any current
        // build is yanked or below the floor, decide() returned Install to force-upgrade OFF
        // it (not Tombstone) — honoring the pin here would keep a revoked/below-floor build
        // running via a purely local pin. In that case the pin is IGNORED and the tuple
        // force-upgrades to the valid pin.
        let all_current_valid = group
            .members
            .iter()
            .all(|m| crate::gate::current_build_ok(ch, m, installed.get(m).copied()));
        if !held.is_empty() && all_current_valid {
            return (TxnOutcome::Pinned(held), BTreeMap::new());
        }
    }

    // GROUP-AGGREGATED DISK PREFLIGHT (§9): one all-or-nothing check for the whole tuple.
    // Only-when-nonempty so an UpToDate group (0 installs) is never spuriously aborted on a
    // low-but-nonzero disk. Fetches only the tiny manifests here; the big asset download
    // stays in stage_member, once.
    let install_members: Vec<&String> = decisions
        .iter()
        .filter(|(_, d)| *d == ApplyDecision::Install)
        .map(|(n, _)| n)
        .collect();
    if !install_members.is_empty()
        && let Some(required) = group_disk_required(fetcher, index, ch, &install_members, triple)
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
        // SAY IT. Disabling a program's tools is the loudest thing this pass can do to a
        // machine, and it was the only tombstone site in this file that reported
        // nothing — while the abort message immediately below told the user the group
        // "stays coherent on its previous builds", which is the opposite of what just
        // happened to these members (2026-08-20 round-13 audit).
        for program in &disabled {
            println!(
                "atpkg: {program} was recalled and there is not room to install its \
                 replacement — its commands are disabled until there is. Free space and \
                 run `atpkg update`."
            );
        }
        // Stage NOTHING — the group stays coherent on its current builds.
        return (
            TxnOutcome::Aborted {
                failed: install_members[0].clone(),
                during_flip: false,
            },
            BTreeMap::new(),
        );
    }

    // Per-group transaction. `staged` is filled by the stage closure and read by flip/rollback.
    let staged: RefCell<BTreeMap<String, Staged>> = RefCell::new(BTreeMap::new());
    let outcome = transact(
        &decisions,
        &mut |m| match stage_member(
            fetcher,
            layout,
            index,
            ch,
            m,
            triple,
            installed.get(m).copied(),
        ) {
            Some(s) => {
                staged.borrow_mut().insert(m.to_string(), s);
                true
            }
            None => false,
        },
        &mut |m| {
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

/// The aggregate installed-bytes a group's Install members need — the sum of each member's
/// signed `size` (compressed asset) + `disk_installed` (extracted tree). Preserves
/// VERIFY-BEFORE-PARSE via the shared [`verified_pkg`] sequence (never parsing unverified
/// TOML). `None` on any verify/parse/fetch failure so the caller's [`disk_gate`] fails
/// OPEN, letting the real stage surface the failure.
fn group_disk_required(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    ch: &Channel,
    install_members: &[&String],
    triple: &str,
) -> Option<u64> {
    let mut total = 0u64;
    for &m in install_members {
        let (_, _, pkg) = verified_pkg(fetcher, index, ch, m)?;
        let a = pkg.artifact_for(triple)?;
        total = total
            .saturating_add(a.size)
            .saturating_add(a.cost.disk_installed);
    }
    Some(total)
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
/// [`plan_update`]), which fetch their candidates directly (no §14 cached fallback).
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
    let cache = crate::cache::IndexCache::new(layout.prefix.join("index-cache.toml"));
    let src = fetcher.cache_source_id();
    match fetcher.index_candidates() {
        Ok(c) if !c.is_empty() => {
            match fetcher.cacheable_candidates(&c) {
                // `store` itself refuses an empty set, so a network leg that succeeded
                // with nothing keeps the older good cache rather than clobbering it.
                // An EMPTY cacheable set takes the union arm below for the same reason
                // `None` does: "the network found no index" and "the network could not
                // be reached" both mean the authoritative leg said nothing this pass.
                Some(cacheable) if !cacheable.is_empty() => {
                    cache.store(&src, &cacheable);
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
                    let mut merged = cache.load(&src).unwrap_or_default();
                    merged.extend(c);
                    Ok(merged)
                }
            }
        }
        // Reached the source, and it genuinely carried no index — a repo with no
        // index release, or a tag pushed off the listing. A trust-shaped answer is
        // the right one here.
        Ok(_) => cache.load(&src).ok_or(FlowError::NoIndex),
        // Could NOT reach it. Keep the reason: telling an offline user their
        // signatures failed sends them to the wrong problem entirely.
        Err(why) => cache.load(&src).ok_or(FlowError::Unreachable(why)),
    }
}

/// Roll `program` back to the highest RETAINED build strictly below its current active build
/// that STILL passes the floor/yank gate (§9/§11). Re-points its shims + the channel
/// `current` to that build via the tested [`rollback_member`] primitive — creating/removing
/// ONLY symlinks, never mutating any retained build's extracted tree (its signed `tree_root`
/// is untouched). The index is resolved + verify-selected so the floor/yank state is
/// authoritative: the target predicate is EXACTLY the one [`decide`] tombstones on
/// (`>= min_build` AND not yanked), so a rollback can never land below the floor or on a
/// revoked build — it errors instead.
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
        .ok_or_else(|| FlowError::Rollback(format!("{program} is not installed/active")))?;
    // 2. Resolve + verify-select the SIGNED index so the floor/yank gate is authoritative
    //    (direct fetch — the single-program paths use no §14 cached fallback).
    let candidates = fetcher.index_candidates().map_err(|_| FlowError::NoIndex)?;
    let index = verify_select_fresh(layout, anchor, candidates, floor, now_unix)?;
    // 3. Reachability — capture the coherence group for the report/warn.
    let coherence_group = index
        .program(program)
        .ok_or_else(|| FlowError::NotReachable(program.to_string()))?
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
    //    predicates decide() tombstones on (>= min_build AND not yanked).
    let target = lower
        .iter()
        .rev()
        .copied()
        .find(|&b| b >= ch.min_build && !crate::gate::is_yanked(ch, program, b))
        .ok_or_else(|| {
            FlowError::Rollback(format!(
                "no retained build below {current} that satisfies the floor/yank gate"
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
/// index + freshness like [`apply_channel`], but from a DIRECT candidate fetch — the single-
/// program paths do not use the §14 cached-index fallback, so a transient index-fetch failure
/// is `NoIndex` rather than a cache fallback. Then find the ONE coherence group containing
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
    floor: BuildFloor,
    now_unix: i64,
) -> Result<ChannelApplyReport, FlowError> {
    let candidates = fetcher.index_candidates().map_err(|_| FlowError::NoIndex)?;
    let index = verify_select_fresh(layout, anchor, candidates, floor, now_unix)?;
    let ch = index
        .channels
        .iter()
        .find(|c| c.name == channel)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?
        .clone();
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
        });
    }
    let mut results = Vec::new();
    let mut applied: BTreeMap<String, AppliedMember> = BTreeMap::new();
    if let Some((acted, o, group_applied)) = apply_group(
        fetcher, layout, &index, &ch, channel, triple, &group, installed,
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
              in, the anchor, the channel + program selectors, the installed build, and \
              the floor + clock the anti-rollback/freshness gates read"
)]
pub fn plan_update(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    channel: &str,
    program: &str,
    installed_build: Option<u64>,
    floor: BuildFloor,
    now_unix: i64,
) -> Result<UpdatePlan, FlowError> {
    let candidates = fetcher.index_candidates().map_err(|_| FlowError::NoIndex)?;
    let index = verify_select_fresh(layout, anchor, candidates, floor, now_unix)?;
    let ch = index
        .channels
        .iter()
        .find(|c| c.name == channel)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    let p = index
        .program(program)
        .ok_or_else(|| FlowError::NotReachable(program.to_string()))?;
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

/// Stage one group member: fetch + verify + parse its per-build manifest, bind program +
/// build, select the artifact (Shim kinds only — sysroot-bundle fails closed), download, and
/// `verify_and_stage` into its build dir. NO activation. `Some(Staged)` on success (with the
/// prior build captured for rollback); `None` on any failure so [`transact`] aborts the group.
fn stage_member(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    ch: &Channel,
    program: &str,
    triple: &str,
    prior_build: Option<u64>,
) -> Option<Staged> {
    let (pinned, repo, pkg) = verified_pkg(fetcher, index, ch, program)?;
    if !pkg.is_for(program) || pkg.build_number != pinned {
        return None;
    }
    let artifact = pkg.artifact_for(triple)?;
    // Shim and sysroot-bundle members stage on this path; app-bundle/unknown fail
    // closed (return None → the group aborts), exactly like `install`.
    let reloc = match crate::dispatch::strategy_for(&artifact.kind) {
        crate::dispatch::ApplyStrategy::Shim => None,
        crate::dispatch::ApplyStrategy::SysrootBundle => Some(artifact.reloc.clone()),
        crate::dispatch::ApplyStrategy::AppBundle | crate::dispatch::ApplyStrategy::Unknown => {
            return None;
        }
    };
    let dl = layout.staging_dir(program).join(&artifact.asset);
    std::fs::create_dir_all(dl.parent()?).ok()?;
    // Same reason as the singleton path: a stale staging entry can be a hardlink
    // into the sealed registry, and fetching over it corrupts the app bundle.
    let _ = std::fs::remove_file(&dl);
    if fetcher
        .download_for(program, &repo, &artifact.asset, &dl)
        .is_err()
    {
        // An aborted transfer still wrote a truncated body here (curl `-o`, no
        // `--remove-on-error`); the retry re-fetches rather than resumes, so it is dead
        // weight. Same reclaim as the stage exit below.
        let _ = std::fs::remove_file(&dl);
        return None;
    }
    let build_dir = layout.build_dir(program, pinned);
    // Capture "this build is ALREADY live" BEFORE the stage swaps a new tree into it, and
    // before any flip can move the links: on a flip-phase abort `flip_member`/`rollback_member`
    // re-point or REMOVE the very links this asks about, so by discard time the answer is
    // gone. `installed` is the shim view and can be silent for a live build (see the abort
    // discard in `apply_group_txn`), which is the whole reason this flag exists.
    let was_live = std::fs::read_link(layout.program_current(program))
        .is_ok_and(|t| t == build_dir)
        || std::fs::read_link(layout.channel_current(&ch.name)).is_ok_and(|t| t == build_dir);
    // Reclaim the compressed asset on EVERY exit, not just the happy one: a group member
    // that fails to stage otherwise strands its archive in `staging/` forever, and nothing
    // else ever sweeps that directory (`gc::interrupted_debris` walks `store/` only).
    let staged = verify_and_stage(artifact, &dl, &build_dir);
    let _ = std::fs::remove_file(&dl);
    staged.ok()?;
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
    if install_tools(layout, &s.build_dir, &s.exposes).is_err() {
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
            for tool in &restore {
                let target = prior_dir.join("bin").join(tool.exe_file());
                if target.exists() {
                    let _ = crate::platform::install_shim(
                        &prior_dir.join("bin"),
                        tool,
                        &layout.shim(tool),
                    );
                } else {
                    // A binary the new build added but the prior build lacks — drop the shim
                    // rather than leave it dangling at a nonexistent prior-build path.
                    let _ = std::fs::remove_file(layout.shim(tool));
                }
            }
            let _ = activate_channel(layout, channel, &prior_dir);
        }
        Some(_) => { /* prior == new build: nothing meaningful to undo */ }
        None => {
            for tool in &s.exposes {
                let _ = std::fs::remove_file(layout.shim(tool));
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
    since_epoch.map_or(i64::MAX, |d| {
        i64::try_from(d.as_secs()).unwrap_or(i64::MAX)
    })
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
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use std::collections::HashMap;
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use crate::sig::testkit;

    /// The synthetic paper master, and the one machine its roster authorizes. The whole
    /// crate signs with this same pair, so a flow test cannot prove something under a
    /// trust shape no other layer uses. (`ROOT_SEED` still names the ROOT of trust — it
    /// is just the paper master now, not a package-specific root.)
    const ROOT_SEED: [u8; 32] = testkit::MASTER_SEED;
    const RELEASE_SEED: [u8; 32] = testkit::MACHINE_SEED;

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
        STANDARD.encode(kp(seed).public_key().as_ref())
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

    // THE COMPRESSED ASSET IS RECLAIMED ON EVERY EXIT, NOT JUST THE HAPPY ONE. A stage that
    // fails otherwise strands a full toolchain archive in `staging/` forever: nothing else
    // sweeps that directory (`gc::interrupted_debris` walks `store/` only), so `atpkg gc`
    // cannot even name the bytes, and a member that keeps failing keeps leaking.
    #[test]
    fn a_failed_stage_reclaims_its_archive_instead_of_stranding_it() {
        let dir = scratch("staging-leak");
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
        for _ in 0..3 {
            let err = install(&tampered, &layout, &anchor(), &req, fl(0), 0).unwrap_err();
            assert!(
                matches!(err, FlowError::Stage(StageError::TreeRootMismatch { .. })),
                "PRECONDITION: the stage must fail AFTER the download: {err:?}"
            );
        }
        assert!(
            staged_assets(&layout, "ay").is_empty(),
            "a failed stage stranded its archive in staging/: {:?}",
            staged_assets(&layout, "ay")
        );

        // Non-vacuity: the archive really does pass through that directory on the way in —
        // a successful install leaves it empty for the same reason, not because nothing
        // was ever downloaded there.
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
            "it must still read as installed, so the retry re-activates instead of re-fetching"
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

    // The group path leaks the same way and must reclaim the same way: an aborted member's
    // archive is the one nobody ever comes back for.
    #[test]
    fn an_aborted_group_member_reclaims_its_archive_too() {
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
        assert!(
            staged_assets(&layout, "ay").is_empty(),
            "and so did the member that staged fine before the abort"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn app_bundle_refused_closed_but_sysroot_bundle_installs() {
        // app-bundle is the notarized self-swap topology, NOT a tool install — it
        // stays refused CLOSED on this path.
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

    #[test]
    fn already_current_is_a_noop() {
        let dir = scratch("current");
        let fake = fixture(&dir);
        let layout = layout(&dir);
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
        assert!(matches!(err, FlowError::NotReachable(_)), "got {err:?}");
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

    // §16.4 dispatch / §16.2 gate: an `app-bundle` artifact (the notarized self-swap path) is
    // refused by `atpkg install` — driven through the two-anchor app-apply gate, which fails
    // closed because notarization is unproven on the CLI install path, before any download.
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
        let mut pkg = HashMap::new();
        let mut archives = HashMap::new();
        for (program, build) in [("trust", 4821u64), ("ay", 18u64)] {
            let archive = prog_archive(dir, program, build);
            let sha = crate::tree::file_sha256(&archive).unwrap();
            let probe = dir.join(format!("probe-{program}"));
            crate::extract::extract_tar_zst(&archive, &probe, 10_000_000, 10_000).unwrap();
            let root = crate::tree::tree_root(&probe).unwrap();
            let asset = format!("{program}-{build}.tar.zst");
            let pkg_body = format!(
                "schema = 2\nprogram = \"{program}\"\nversion = \"0.1\"\nbuild_number = {build}\n\
                 exposes = [\"{program}\"]\n\
                 [[artifact]]\ntarget = \"{TRIPLE}\"\nkind = \"binary\"\nasset = \"{asset}\"\n\
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
            fl(0),
            0,
        )
        .unwrap();
        let ay18 = layout.build_dir("ay", 18);
        assert!(
            crate::store::build_is_complete(&ay18),
            "PRECONDITION: ay@18 is installed and complete"
        );

        // 2. `atpkg unlink`-shaped state: ay's shim is gone, so the SHIM view is silent for
        //    ay — while the authority link still names ay@18 as live.
        std::fs::remove_file(layout.shim(&tool("ay"))).unwrap();
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
            crate::activate::install_shims(layout, &dir, &[program.to_string()]).unwrap();
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
                    for removed in [
                        &[][..],
                        &["ay"][..],
                        &["trust"][..],
                        &["ay", "trust"][..],
                    ] {
                        let label =
                            format!("{cname}-{ay:?}-t{trust_installed}-r{}", removed.len());
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
                        if stale_only && cname == "yank-installed" && matches!(ay, Ay::Revoked) {
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
        assert!(checked >= 24, "the enumeration ran only {checked} cases");
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
                          [programs.ay]\nrepo = \"ay\"\n[programs.ny]\nrepo = \"ny\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             {yanked_toml}pin = {{ ay = 18, ny = 9 }}\n",
            attr = attribution()
        );
        Fake {
            index: index_body.clone().into_bytes(),
            index_sig: sign(&RELEASE_SEED, index_body.as_bytes()),
            pkg,
            archives,
        }
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
        let cache = crate::cache::IndexCache::new(layout.prefix.join("index-cache.toml"));
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
        install_tools(&l, &b18, &[tool("ay"), tool("aylint")]).unwrap();
        activate_channel(&l, "stable", &b19).unwrap();
        install_tools(&l, &b19, &[tool("ay")]).unwrap();
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
        install_tools(&l, &b18, &[tool("ay")]).unwrap();
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












}
