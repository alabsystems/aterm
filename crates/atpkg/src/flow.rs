// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The end-to-end install orchestration (§5/§7/§8/§9) — composing every verified
//! primitive into one program install, with the **network abstracted behind
//! [`Fetcher`]** so the whole sequence is unit-testable against synthetic *signed*
//! fixtures (no real release needed). The production [`Fetcher`] is a thin adapter over
//! `aterm-update-core`'s authenticated `curl` plumbing.
//!
//! The ordered, fail-closed pipeline ([`install`]):
//! 1. fetch index candidates → [`crate::select::select_index`] (verify-then-select, §5);
//! 2. **reachability** — the program must be named in the verified index (§5), and pinned
//!    in the requested channel;
//! 3. [`crate::gate::decide`] — `UpToDate`/`Tombstone`/`NotPinned` short-circuit;
//! 4. fetch the per-build `pkg.toml`, [`verify_pkg`] under the index's delegated release
//!    key, [`parse_pkg`], and check its signed `program`/`build_number` bind the request
//!    (anti-replay, §4.2);
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
use crate::sig::verify_pkg;
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
// calls (no `write!`): the `write!`/`format_args!` expansion embeds `fmt::Arguments`
// construction (with inlined `unsafe`) that the strict Trust gate cannot lower and
// fails closed on. Byte-identical output (`write!` with `{}`/`{:?}` args performs
// exactly these formatter writes in sequence; no width/fill flags are used).
impl std::fmt::Display for FlowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlowError::NoIndex => f.write_str("no signature-valid index at/above the floor"),
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
            // Best-of-both additions (write_str style, Trust-gate-safe — no `write!`):
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
/// I/O, `root_pubkey_b64` as the pinned root key, and `floor` as the durable index
/// high-water. See the module docs for the ordered, fail-closed pipeline.
pub fn install(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    root_pubkey_b64: &str,
    req: &InstallRequest,
    floor: u64,
    now_unix: i64,
) -> Result<InstallReport, FlowError> {
    let mut seen = BTreeSet::new();
    install_inner(
        fetcher,
        layout,
        root_pubkey_b64,
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
    root_pubkey_b64: &str,
    req: &InstallRequest,
    floor: u64,
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
    // 1–2. Resolve + verify-select the index (cached-fallback, §14), then reachability.
    let candidates = resolve_candidates(fetcher, layout)?;
    let selected = select_index(root_pubkey_b64, candidates, floor).ok_or(FlowError::NoIndex)?;
    let index = selected.index;
    // Freshness (§8 gate 2): refuse a selected index whose window has lapsed. A
    // valid_until we cannot parse is treated as lapsed (fail closed).
    match rfc3339_to_unix(&index.valid_until) {
        Some(until) if crate::sig::check_freshness(now_unix, until).is_ok() => {}
        _ => return Err(FlowError::Stale),
    }
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
    let verified = verify_pkg(raw, &sig, &index.delegation()).map_err(|_| FlowError::PkgVerify)?;
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
    for dep in &pkg.requires {
        if dep.as_str() == program || seen.contains(dep) {
            dependencies.push(DepOutcome {
                program: dep.clone(),
                result: DepResult::Skipped("already resolved or cycle".into()),
            });
            continue;
        }
        if let Some(b) = crate::ops::active_builds(layout).get(dep).copied() {
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
            root_pubkey_b64,
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
    // fail-loud resolve check AFTER — so a broken toolchain aborts instead of being
    // laid down and reported SUCCESS. `app-bundle` (notarized self-swap) and unknown
    // kinds remain refused CLOSED. (audit: sysroot-bundle silent-broken-install.)
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
    fetcher
        .download_for(program, &repo, &artifact.asset, &dl)
        .map_err(FlowError::Download)?;
    let build_dir = layout.build_dir(program, pinned);
    // Preflight again before extract: the asset is already downloaded, so only the extracted
    // tree remains to fit.
    disk_gate(
        artifact.cost.disk_installed,
        crate::freespace::available_bytes(&build_dir),
    )?;
    verify_and_stage(artifact, &dl, &build_dir).map_err(FlowError::Stage)?;
    let _ = std::fs::remove_file(&dl); // reclaim the compressed asset

    // 6b. Sysroot-bundle wiring BEFORE activation (self-contained = no-op).
    if strategy == crate::dispatch::ApplyStrategy::SysrootBundle {
        apply_sysroot_bundle(&artifact.reloc)?;
    }

    // 7. Activate + shim. The raw manifest `exposes` is admitted ONCE here; `tools` is what
    // actually got a shim and `refused` the sensitive/malformed names that did not.
    activate_channel(layout, channel, &build_dir)
        .map_err(|e| FlowError::Activate(e.to_string()))?;
    let (tools, refused) = crate::store::split_exposed(&pkg.exposes);
    install_tools(layout, &build_dir, &tools).map_err(|e| FlowError::Activate(e.to_string()))?;

    // 7b. Fail-loud resolve check: an installed sysroot-bundle's compilers must
    // actually load. A dynamic-loader failure here aborts the install.
    if strategy == crate::dispatch::ApplyStrategy::SysrootBundle {
        bundle_resolve_check(&build_dir, &tools)?;
    }
    // NOTE: the shell.d hook refresh runs at the main.rs CLI edge (do_install / cmd_update),
    // NOT here — writing ~/.aterm from flow's synthetic-layout unit tests would pollute the
    // developer's real home (identical hermeticity reasoning as the GC-after-activate edge).
    let shimmed: Vec<String> = tools.iter().map(|t| t.as_str().to_string()).collect();
    Ok(InstallReport {
        program: program.to_string(),
        build: pinned,
        index_build: index.index_build,
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
    /// Holding [`ToolName`]s here rather than `Vec<String>` is what forces `rollback_member`
    /// below to say `exe_file()` when it probes the prior build for a binary; it used to join
    /// the bare name, so on Windows the probe missed `ay.exe` and the rollback dropped the
    /// shim of a tool the prior build had all along.
    exposes: Vec<ToolName>,
    /// The member's prior active build (`None` ⇒ a fresh install: rollback removes the shims).
    prior_build: Option<u64>,
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
    root_pubkey_b64: &str,
    channel: &str,
    triple: &str,
    installed: &BTreeMap<String, u64>,
    floor: u64,
    now_unix: i64,
) -> Result<ChannelApplyReport, FlowError> {
    // 1–2. Resolve + verify-select the index ONCE (cached-fallback, §14), then freshness (§8).
    let candidates = resolve_candidates(fetcher, layout)?;
    let selected = select_index(root_pubkey_b64, candidates, floor).ok_or(FlowError::NoIndex)?;
    let index = selected.index;
    match rfc3339_to_unix(&index.valid_until) {
        Some(until) if crate::sig::check_freshness(now_unix, until).is_ok() => {}
        _ => return Err(FlowError::Stale),
    }
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
        if let Some((outcome, group_applied)) = apply_group(
            fetcher, layout, &index, &ch, channel, triple, group, installed,
        ) {
            applied.extend(group_applied);
            results.push((group.clone(), outcome));
        }
    }
    // (Shell.d hook refresh runs at the main.rs CLI edge, not here — see the note in
    // `install` — to keep apply_channel's unit tests hermetic w.r.t. the real ~/.aterm.)
    Ok(ChannelApplyReport {
        index_build: index.index_build,
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
    index: &Index,
    ch: &Channel,
    channel: &str,
    triple: &str,
    group: &Group,
    installed: &BTreeMap<String, u64>,
) -> Option<(TxnOutcome, BTreeMap<String, AppliedMember>)> {
    // `update` touches INSTALLED groups only: skip a group with no installed member (that
    // would be a fresh `install`, not an update). A coherence group with even ONE member
    // installed IS processed in full — the locked tuple must stay coherent, so a missing
    // sibling is pulled in to the pin (decide → Install).
    if group.members.iter().all(|m| !installed.contains_key(m)) {
        return None;
    }
    Some(apply_group_txn(
        fetcher, layout, index, ch, channel, triple, group, installed,
    ))
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
    index: &Index,
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
/// parse is identical to [`stage_member`]/[`group_disk_required`].
pub fn group_missing_triple(
    fetcher: &dyn Fetcher,
    index: &Index,
    channel: &str,
    triple: &str,
    members: &[String],
) -> Option<String> {
    let ch = index.channels.iter().find(|c| c.name == channel)?;
    for m in members {
        let Some(&pinned) = ch.pin.get(m.as_str()) else {
            continue;
        };
        let Some(p) = index.program(m) else {
            continue;
        };
        let Ok((raw, sig)) = fetcher.pkg_manifest(&p.repo, m, pinned) else {
            continue;
        };
        let Ok(verified) = verify_pkg(raw, &sig, &index.delegation()) else {
            continue;
        };
        let Ok(pkg) = parse_pkg(&verified) else {
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
    index: &Index,
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
    // process each staged `build_dir` is the NEW pinned build (decide only stages Install
    // members, so new != the prior active build); across processes the discard rests on the
    // SINGLE-WRITER-PER-STORE contract ([`crate::lock`]): every mutating verb try-acquires
    // the store-wide `store.lock` at the CLI edge, so no OTHER atpkg process can be staging
    // or activating builds in this store while this transaction runs — without that lock, a
    // concurrent process could have just activated one of these very builds, and this
    // discard would leave its shims dangling on a deleted tree.
    if matches!(outcome, TxnOutcome::Aborted { .. }) {
        for s in staged.borrow().values() {
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
/// VERIFY-BEFORE-PARSE: it runs the IDENTICAL verify_pkg → parse_pkg sequence as
/// [`stage_member`], never parsing unverified TOML. `None` on any verify/parse/fetch failure
/// so the caller's [`disk_gate`] fails OPEN, letting the real stage surface the failure.
fn group_disk_required(
    fetcher: &dyn Fetcher,
    index: &Index,
    ch: &Channel,
    install_members: &[&String],
    triple: &str,
) -> Option<u64> {
    let mut total = 0u64;
    for &m in install_members {
        let pinned = *ch.pin.get(m.as_str())?;
        let repo = index.program(m)?.repo.clone();
        let (raw, sig) = fetcher.pkg_manifest(&repo, m, pinned).ok()?;
        let verified = verify_pkg(raw, &sig, &index.delegation()).ok()?; // verify FIRST
        let pkg = parse_pkg(&verified).ok()?; // parse only &VerifiedBytes
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
    root_pubkey_b64: &str,
    floor: u64,
    now_unix: i64,
) -> Result<Index, FlowError> {
    let candidates = resolve_candidates(fetcher, layout)?;
    let selected = select_index(root_pubkey_b64, candidates, floor).ok_or(FlowError::NoIndex)?;
    let index = selected.index;
    match rfc3339_to_unix(&index.valid_until) {
        Some(until) if crate::sig::check_freshness(now_unix, until).is_ok() => {}
        _ => return Err(FlowError::Stale),
    }
    Ok(index)
}

/// Resolve the index candidates with a SAME-SOURCE cached fallback (§14): a successful fetch
/// refreshes the cache under the fetcher's `source_id`; a fetch failure falls back to the last
/// cached candidates FOR THE SAME SOURCE (a `dir:` cache never satisfies a failed `github:`
/// fetch). Cached bytes are RAW — everything downstream (verify-then-select, freshness, floor)
/// is unchanged, so a tampered/stale cache installs nothing the live path wouldn't.
fn resolve_candidates(fetcher: &dyn Fetcher, layout: &Layout) -> Result<Vec<Candidate>, FlowError> {
    let cache = crate::cache::IndexCache::new(layout.prefix.join("index-cache.toml"));
    let src = fetcher.source_id();
    match fetcher.index_candidates() {
        Ok(c) => {
            cache.store(&src, &c);
            Ok(c)
        }
        Err(_) => cache.load(&src).ok_or(FlowError::NoIndex),
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
    root_pubkey_b64: &str,
    channel: &str,
    program: &str,
    floor: u64,
    now_unix: i64,
) -> Result<RollbackReport, FlowError> {
    // 1. The ACTIVE build (shim-derived), never a merely-staged one.
    let current = *crate::ops::active_builds(layout)
        .get(program)
        .ok_or_else(|| FlowError::Rollback(format!("{program} is not installed/active")))?;
    // 2. Resolve + verify-select the SIGNED index so the floor/yank gate is authoritative.
    let candidates = fetcher.index_candidates().map_err(|_| FlowError::NoIndex)?;
    let selected = select_index(root_pubkey_b64, candidates, floor).ok_or(FlowError::NoIndex)?;
    let index = selected.index;
    match rfc3339_to_unix(&index.valid_until) {
        Some(until) if crate::sig::check_freshness(now_unix, until).is_ok() => {}
        _ => return Err(FlowError::Stale),
    }
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
        reloc: None,
        tree_root: String::new(),
    };
    rollback_member(layout, channel, program, &staged);
    Ok(RollbackReport {
        program: program.to_string(),
        from_build: current,
        to_build: target,
        index_build: index.index_build,
        coherence_group,
    })
}

/// The transactional `update <grouped-member>` path (§11 tuple-split fix): resolve + verify-
/// select the index (same prologue as [`apply_channel`]), find the ONE coherence group
/// containing `program`, and apply THAT WHOLE group atomically via [`apply_group`]. A grouped
/// member therefore stages-all → flips-all → rolls-back atomically and can NEVER move
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
    root_pubkey_b64: &str,
    channel: &str,
    triple: &str,
    program: &str,
    installed: &BTreeMap<String, u64>,
    floor: u64,
    now_unix: i64,
) -> Result<ChannelApplyReport, FlowError> {
    let candidates = fetcher.index_candidates().map_err(|_| FlowError::NoIndex)?;
    let selected = select_index(root_pubkey_b64, candidates, floor).ok_or(FlowError::NoIndex)?;
    let index = selected.index;
    match rfc3339_to_unix(&index.valid_until) {
        Some(until) if crate::sig::check_freshness(now_unix, until).is_ok() => {}
        _ => return Err(FlowError::Stale),
    }
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
            groups: vec![],
            applied: BTreeMap::new(),
            skipped_linked,
        });
    }
    let mut results = Vec::new();
    let mut applied: BTreeMap<String, AppliedMember> = BTreeMap::new();
    if let Some((o, group_applied)) = apply_group(
        fetcher, layout, &index, &ch, channel, triple, &group, installed,
    ) {
        applied.extend(group_applied);
        results.push((group, o));
    }
    // (Shell.d hook refresh runs at the main.rs CLI edge — see the note in `install`.)
    Ok(ChannelApplyReport {
        index_build: index.index_build,
        groups: results,
        applied,
        skipped_linked: vec![],
    })
}

/// Read-only routing decision for the `update` verb (§11): resolve + verify-select the index
/// (verify-before-parse), then return `program`'s coherence group (to pick the transactional-
/// vs-single path) AND the authoritative [`decide`] result (so an ungrouped pin gate can be
/// applied strictly AFTER it, never hiding a Tombstone). Pure read — no staging/mutation.
pub fn plan_update(
    fetcher: &dyn Fetcher,
    root_pubkey_b64: &str,
    channel: &str,
    program: &str,
    installed_build: Option<u64>,
    floor: u64,
    now_unix: i64,
) -> Result<UpdatePlan, FlowError> {
    let candidates = fetcher.index_candidates().map_err(|_| FlowError::NoIndex)?;
    let selected = select_index(root_pubkey_b64, candidates, floor).ok_or(FlowError::NoIndex)?;
    let index = selected.index;
    match rfc3339_to_unix(&index.valid_until) {
        Some(until) if crate::sig::check_freshness(now_unix, until).is_ok() => {}
        _ => return Err(FlowError::Stale),
    }
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

/// Stage one group member: fetch + verify + parse its per-build manifest, bind program +
/// build, select the artifact (Shim kinds only — sysroot-bundle fails closed), download, and
/// `verify_and_stage` into its build dir. NO activation. `Some(Staged)` on success (with the
/// prior build captured for rollback); `None` on any failure so [`transact`] aborts the group.
fn stage_member(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &Index,
    ch: &Channel,
    program: &str,
    triple: &str,
    prior_build: Option<u64>,
) -> Option<Staged> {
    let pinned = *ch.pin.get(program)?;
    let repo = index.program(program)?.repo.clone();
    let (raw, sig) = fetcher.pkg_manifest(&repo, program, pinned).ok()?;
    let verified = verify_pkg(raw, &sig, &index.delegation()).ok()?;
    let pkg = parse_pkg(&verified).ok()?;
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
    fetcher
        .download_for(program, &repo, &artifact.asset, &dl)
        .ok()?;
    let build_dir = layout.build_dir(program, pinned);
    verify_and_stage(artifact, &dl, &build_dir).ok()?;
    let _ = std::fs::remove_file(&dl);
    Some(Staged {
        build: pinned,
        build_dir,
        // Refused (sensitive/malformed) names are dropped here rather than at flip time —
        // same outcome as before, one admission instead of one per flip/rollback pass. A
        // group member's refusals are not a stage failure, matching `install`.
        exposes: crate::store::split_exposed(&pkg.exposes).0,
        prior_build,
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
/// ([`ToolName::exe_file`]), not the logical tool: it used to join the bare name, which on
/// Windows never matched `ay.exe`, so every tool looked absent and the rollback deleted shims
/// it should have re-pointed — leaving the user with no `PATH` entry at all after a failed
/// update. The `shim_allowed` calls this loop used to make are gone because a `ToolName`
/// cannot exist without them.
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

/// Parse an RFC3339 UTC timestamp `YYYY-MM-DDTHH:MM:SSZ` to a Unix epoch second. Pure (no
/// clock), so the freshness gate stays deterministic; `None` on any malformed field, which
/// the caller treats as lapsed (fail closed). Uses the standard `days_from_civil` algorithm.
pub(crate) fn rfc3339_to_unix(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
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
    // days_from_civil (Howard Hinnant): days since 1970-01-01.
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = (if yy >= 0 { yy } else { yy - 399 }) / 400;
    let yoe = yy - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
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

    const ROOT_SEED: [u8; 32] = [7u8; 32];
    const RELEASE_SEED: [u8; 32] = [1u8; 32];
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
        tar.extend(entry("bin/ay", b"#!/bin/true\nay"));
        tar.extend(entry("bin/git", b"#!/bin/true\nnot-really-git")); // sensitive → refused shim
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
            Ok(vec![Candidate {
                label: "v0".into(),
                index_bytes: self.index.clone(),
                sig: self.index_sig.clone(),
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
        let archive = make_archive(dir);
        let sha = crate::tree::file_sha256(&archive).unwrap();
        // Learn the extracted tree_root by a throwaway stage.
        let probe = dir.join("probe");
        crate::extract::extract_tar_zst(&archive, &probe, 10_000_000, 10_000).unwrap();
        let root = crate::tree::tree_root(&probe).unwrap();

        let index_body = format!(
            "schema = 1\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
             [keys]\nrelease_key_id = \"rk\"\nrelease_key_pubkey = \"{rk}\"\n\
             [programs.ay]\nrepo = \"ay\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             pin = {{ ay = 18 }}\n",
            rk = pk(&RELEASE_SEED)
        );
        let pkg_body = format!(
            "schema = 1\nprogram = \"ay\"\nversion = \"0.1\"\nbuild_number = 18\n\
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
            index_sig: sign(&ROOT_SEED, index_body.as_bytes()),
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
        let report = install(&fake, &layout, &pk(&ROOT_SEED), &req, 0, 0).unwrap();
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
        let err = install(&app, &alay, &pk(&ROOT_SEED), &areq, 0, 0).unwrap_err();
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
        let rep = install(&sr, &slay, &pk(&ROOT_SEED), &sreq, 0, 0)
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
        let err = install(&fake, &layout, &pk(&ROOT_SEED), &req, 0, 0).unwrap_err();
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
        let r = install(&fake, &layout, &pk(&ROOT_SEED), &req, 0, 0).unwrap();
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
        let err = install(&fake, &layout, &pk(&ROOT_SEED), &req, 0, 0).unwrap_err();
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
        let err = install(&fake, &layout, &pk(&RELEASE_SEED), &req, 0, 0).unwrap_err();
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
        let err = install(&fake, &layout, &pk(&ROOT_SEED), &req, 0, 0).unwrap_err();
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
        let err = install(&fake, &layout, &pk(&ROOT_SEED), &req, 0, 2_000_000_000).unwrap_err();
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
            "schema = 1\nindex_build = 41\nvalid_until = \"2099-01-01T00:00:00Z\"\n\
             [keys]\nrelease_key_id = \"rk\"\nrelease_key_pubkey = \"{}\"\n\
             [programs.aterm]\nrepo = \"aterm\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\npin = {{ aterm = 18 }}\n",
            pk(&RELEASE_SEED)
        );
        let pkg_body = format!(
            "schema = 1\nprogram = \"aterm\"\nbuild_number = 18\nexposes = []\n\
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
            index_sig: sign(&ROOT_SEED, index_body.as_bytes()),
            pkg,
            archives: HashMap::new(),
        };
        let req = InstallRequest {
            channel: "stable",
            program: "aterm",
            triple: TRIPLE,
            installed: None,
        };
        let err = install(&fake, &layout(&dir), &pk(&ROOT_SEED), &req, 0, 0).unwrap_err();
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
                "schema = 1\nprogram = \"{program}\"\nversion = \"0.1\"\nbuild_number = {build}\n\
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
            "schema = 1\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
             [keys]\nrelease_key_id = \"rk\"\nrelease_key_pubkey = \"{rk}\"\n\
             [programs.trust]\nrepo = \"trust\"\ncoherence_group = \"rustc\"\n\
             [programs.ay]\nrepo = \"ay\"\ncoherence_group = \"rustc\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             pin = {{ trust = 4821, ay = 18 }}\n",
            rk = pk(&RELEASE_SEED)
        );
        Fake {
            index: index_body.clone().into_bytes(),
            index_sig: sign(&ROOT_SEED, index_body.as_bytes()),
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
            &pk(&ROOT_SEED),
            "stable",
            TRIPLE,
            &installed,
            0,
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
            &pk(&ROOT_SEED),
            "stable",
            TRIPLE,
            &installed,
            0,
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
            &pk(&ROOT_SEED),
            "stable",
            TRIPLE,
            &installed,
            0,
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
            "schema = 1\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
             [keys]\nrelease_key_id = \"rk\"\nrelease_key_pubkey = \"{rk}\"\n\
             [programs.ay]\nrepo = \"ay\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = {min_build}\n\
             {yanked_toml}pin = {{ ay = 18 }}\n",
            rk = pk(&RELEASE_SEED)
        );
        Fake {
            index: index_body.clone().into_bytes(),
            index_sig: sign(&ROOT_SEED, index_body.as_bytes()),
            pkg: HashMap::new(),
            archives: HashMap::new(),
        }
    }

    /// [`group_fixture`] but whose stable channel YANKS `ay@18`, so `decide` tombstones the
    /// group (to prove a pin never suppresses a tombstone).
    fn group_fixture_yanking_ay18(dir: &Path) -> Fake {
        let mut f = group_fixture(dir);
        let index_body = format!(
            "schema = 1\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
             [keys]\nrelease_key_id = \"rk\"\nrelease_key_pubkey = \"{rk}\"\n\
             [programs.trust]\nrepo = \"trust\"\ncoherence_group = \"rustc\"\n\
             [programs.ay]\nrepo = \"ay\"\ncoherence_group = \"rustc\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             yanked = [\"ay@18\"]\n\
             pin = {{ trust = 4821, ay = 18 }}\n",
            rk = pk(&RELEASE_SEED)
        );
        f.index = index_body.clone().into_bytes();
        f.index_sig = sign(&ROOT_SEED, index_body.as_bytes());
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
            &pk(&ROOT_SEED),
            "stable",
            TRIPLE,
            &installed,
            0,
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
            "schema = 1\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
             [keys]\nrelease_key_id = \"rk\"\nrelease_key_pubkey = \"{rk}\"\n\
             [programs.trust]\nrepo = \"trust\"\ncoherence_group = \"rustc\"\n\
             [programs.ay]\nrepo = \"ay\"\ncoherence_group = \"rustc\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             yanked = [\"ay@17\"]\n\
             pin = {{ trust = 4821, ay = 18 }}\n",
            rk = pk(&RELEASE_SEED)
        );
        f.index = index_body.clone().into_bytes();
        f.index_sig = sign(&ROOT_SEED, index_body.as_bytes());
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
            &pk(&ROOT_SEED),
            "stable",
            TRIPLE,
            &installed,
            0,
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
            &pk(&ROOT_SEED),
            "stable",
            TRIPLE,
            &installed,
            0,
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
        let r = rollback(&fake, &layout, &pk(&ROOT_SEED), "stable", "ay", 0, 0).unwrap();
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
                &pk(&ROOT_SEED),
                "stable",
                "ay",
                0,
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
                &pk(&ROOT_SEED),
                "stable",
                "ay",
                0,
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
                &pk(&ROOT_SEED),
                "stable",
                "ay",
                0,
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
        let err = rollback(&fake, &layout, &pk(&ROOT_SEED), "stable", "ay", 0, 0).unwrap_err();
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
            &pk(&ROOT_SEED),
            "stable",
            TRIPLE,
            "ay",
            &installed,
            0,
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
                "schema = 1\nprogram = \"{program}\"\nversion = \"0.1\"\nbuild_number = {build}\n\
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
            "schema = 1\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
             [keys]\nrelease_key_id = \"rk\"\nrelease_key_pubkey = \"{rk}\"\n\
             [programs.ay]\nrepo = \"ay\"\n[programs.ny]\nrepo = \"ny\"\n\
             [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
             {yanked_toml}pin = {{ ay = 18, ny = 9 }}\n",
            rk = pk(&RELEASE_SEED)
        );
        Fake {
            index: index_body.clone().into_bytes(),
            index_sig: sign(&ROOT_SEED, index_body.as_bytes()),
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
        let report = install(&fake, &layout, &pk(&ROOT_SEED), &req, 0, 0).unwrap();
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
        let report = install(&fake, &layout, &pk(&ROOT_SEED), &req, 0, 0).unwrap();
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
        let report = install(&fake, &layout, &pk(&ROOT_SEED), &req, 0, 0).unwrap();
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
        install(&fake, &layout, &pk(&ROOT_SEED), &ny, 0, 0).unwrap();
        // Now ay requires ny, which is already active.
        let ay = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        let report = install(&fake, &layout, &pk(&ROOT_SEED), &ay, 0, 0).unwrap();
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
        let report = install(&fake, &layout, &pk(&ROOT_SEED), &req, 0, 0).unwrap();
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
        let report = install(&fake, &layout, &pk(&ROOT_SEED), &req, 0, 0).unwrap();
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
                "schema = 1\nprogram = \"{program}\"\ncheckout = \"/nonexistent\"\nbins = []\n"
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
        let err = install(&fake, &layout, &pk(&ROOT_SEED), &req, 0, 0).unwrap_err();
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
            &pk(&ROOT_SEED),
            "stable",
            TRIPLE,
            &installed,
            0,
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

    /// A [`Fake`] wrapper whose index fetch can be toggled to fail, with a controllable
    /// `source_id` (to exercise the same-source cache guard).
    struct FlakyFake {
        inner: Fake,
        fail: std::cell::Cell<bool>,
        source: String,
    }
    impl Fetcher for FlakyFake {
        fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
            if self.fail.get() {
                Err("network down".into())
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
        let f = FlakyFake {
            inner: fixture(&dir),
            fail: std::cell::Cell::new(false),
            source: "src:A".into(),
        };
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        // 1. A good fetch installs AND caches the index under source "src:A".
        install(&f, &layout, &pk(&ROOT_SEED), &req, 0, 0).unwrap();
        // 2. Fetch now fails, SAME source → the install is served from the cache.
        f.fail.set(true);
        install(&f, &layout, &pk(&ROOT_SEED), &req, 0, 0).expect("cache fallback serves the index");
        // 3. A DIFFERENT source with a failing fetch has no cache → NoIndex.
        let f2 = FlakyFake {
            inner: fixture(&dir),
            fail: std::cell::Cell::new(true),
            source: "src:B".into(),
        };
        let err = install(&f2, &layout, &pk(&ROOT_SEED), &req, 0, 0).unwrap_err();
        assert!(
            matches!(err, FlowError::NoIndex),
            "a dir: cache never satisfies a github: fetch"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_cached_index_is_still_refused() {
        let dir = scratch("cache-stale");
        let layout = layout(&dir);
        let f = FlakyFake {
            inner: fixture(&dir),
            fail: std::cell::Cell::new(false),
            source: "src:A".into(),
        };
        let req = InstallRequest {
            channel: "stable",
            program: "ay",
            triple: TRIPLE,
            installed: None,
        };
        // A `now` past valid_until: even a good fetch is refused Stale — but the bytes are cached.
        assert!(matches!(
            install(&f, &layout, &pk(&ROOT_SEED), &req, 0, 2_000_000_000),
            Err(FlowError::Stale)
        ));
        // The fetch now fails → fallback to the cached bytes, which are STILL past valid_until.
        f.fail.set(true);
        assert!(
            matches!(
                install(&f, &layout, &pk(&ROOT_SEED), &req, 0, 2_000_000_000),
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
        let report = install(&df, &layout, &pk(&ROOT_SEED), &req, 0, 0).unwrap();
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
        let err = install(&df2, &layout, &pk(&RELEASE_SEED), &req, 0, 0).unwrap_err();
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
}
