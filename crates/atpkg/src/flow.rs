// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! End-to-end verified package install orchestration ([`install`]) over [`Fetcher`].

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[cfg(test)]
use crate::activate::install_tools;
use crate::activate::{Aliases, activate_build, install_tombstone_shim, install_tools_env};
use crate::apply::{Group, TxnOutcome, transact_holding};
use crate::gate::{ApplyDecision, decide};
use crate::install::StageError;
use crate::manifest::{Channel, Index, parse_pkg};
use crate::select::{Candidate, select_index};
use crate::sig::{Anchor, BuildFloor, TrustedIndex};
use crate::store::{Layout, ToolName};

pub(crate) mod ahead;

fn verify_and_stage(
    artifact: &crate::manifest::Artifact,
    archive: &Path,
    build_dir: &Path,
    hooks: &crate::install::StageHooks<'_>,
) -> Result<String, StageError> {
    let staged = crate::install::verify_and_stage(artifact, archive, build_dir, hooks);
    if staged.is_err() && !build_dir.exists() {
        crate::shim_env::remove_sidecar(build_dir);
    }
    staged
}

fn alab_pack_gate(program: &str, exposes: &[String]) -> Option<Box<StageGate>> {
    crate::alab::agent(program).map(|_| -> Box<StageGate> {
        let pack = crate::install::alab_pack_gate(exposes, crate::install::codesign_strict_check);
        Box::new(move |tree: &Path| {
            pack(tree)?;
            crate::alab::pairs_with(tree, crate::alab::this_aterm()).map_err(StageError::Payload)
        })
    })
}

type StageGate = dyn Fn(&Path) -> Result<(), StageError>;

fn stage_hooks(gate: Option<&StageGate>) -> crate::install::StageHooks<'_> {
    crate::install::StageHooks {
        pre_swap: gate,
        sidecars: None,
        aliases: &[],
        npm_package: None,
    }
}

/// What [`Fetcher::vendor_get`] came back with (mirrors `aterm_update_core::VendorResponse`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VendorGet {
    /// HTTP 200: the whole body (at most the cap).
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
        effective_url: String,
    },
    /// HTTP 304 to a conditional request.
    NotModified { etag: String },
}

/// Why a vendor-direct fetch produced nothing, split the way the lane must treat it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VendorFetchError {
    /// The vendor host was not reached or is not serving now.
    Unreachable(String),
    /// A verdict about the request or the document.
    Refused(String),
}

impl std::fmt::Display for VendorFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(message) | Self::Refused(message) => f.write_str(message),
        }
    }
}

/// The network operations the install flow needs, abstracted so the orchestration is testable.
pub trait Fetcher {
    fn index_candidates(&self) -> Result<Vec<Candidate>, String>;
    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String>;
    fn prefetch_manifests(&self, _pins: &[(String, String, u64)]) {}
    fn shared(&self) -> Option<&(dyn Fetcher + Sync)> {
        None
    }
    fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String>;
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
    fn download_url(&self, url: &str, dest: &Path, cap: u64) -> Result<(), String> {
        let _ = (url, dest, cap);
        Err(
            "this fetcher cannot fetch vendor URLs (the https protocol is network-only)"
                .to_string(),
        )
    }
    fn vendor_get(
        &self,
        program: &str,
        url: &str,
        cap: u64,
        if_none_match: Option<&str>,
    ) -> Result<VendorGet, VendorFetchError> {
        let _ = (program, url, cap, if_none_match);
        Err(VendorFetchError::Unreachable(
            "this fetcher cannot fetch vendor documents (the vendor-direct lane is network-only)"
                .to_string(),
        ))
    }
    fn vendor_head(
        &self,
        program: &str,
        url: &str,
        cap: u64,
        if_none_match: Option<&str>,
    ) -> Result<VendorGet, VendorFetchError> {
        self.vendor_get(program, url, cap, if_none_match)
    }
    fn vendor_content_length(&self, program: &str, url: &str) -> Result<u64, VendorFetchError> {
        let _ = (program, url);
        Err(VendorFetchError::Unreachable(
            "this fetcher cannot size vendor payloads (the vendor-direct lane is network-only)"
                .to_string(),
        ))
    }
    fn vendor_download(
        &self,
        program: &str,
        url: &str,
        dest: &Path,
        cap: u64,
    ) -> Result<(), VendorFetchError> {
        let _ = (program, url, dest, cap);
        Err(VendorFetchError::Unreachable(
            "this fetcher cannot download vendor payloads (the vendor-direct lane is network-only)"
                .to_string(),
        ))
    }
    fn source_id(&self) -> String {
        "fetcher:unspecified".to_string()
    }
    fn index_link_down(&self) -> bool {
        false
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
    /// durable high-water [`crate::sig::Floor`] to this on success.
    pub index_build: u64,
    /// The `roster_seq` of the master-signed generation that authorized that index.
    pub roster_seq: u64,
    /// Whether it was already current (no build moved).
    pub already_current: bool,
    /// The `bin/` shims installed (when already current, the commands put back on PATH).
    pub shimmed: Vec<String>,
    /// `exposes` names refused a shim (sensitive-name collisions).
    pub refused_shims: Vec<String>,
    /// The signed `tree_root` of the installed build.
    pub tree_root: String,
    /// Required-dependency pull-in outcomes (flattened transitive closure), with yanked.
    pub dependencies: Vec<DepOutcome>,
    /// `Some` when the program is vendor-direct.
    pub vendor: Option<VendorReport>,
}

/// What the vendor-direct lane did for an `install`/`update` of one of its programs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorReport {
    /// The version now active, when one is.
    pub version: Option<String>,
    /// The vendor, as its row names it (`Anthropic`).
    pub vendor: String,
    /// The verdict line (`atpkg: claude 2.1.278 → 2.1.280 (Anthropic latest)`).
    pub line: String,
}

/// One resolved `requires` dependency and what happened to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepOutcome {
    /// The dependency program name.
    pub program: String,
    /// The pull-in result.
    pub result: DepResult,
}

/// The outcome of resolving one `requires` dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepResult {
    /// Freshly installed at `build`, carrying its signed `tree_root` so the CLI records it.
    Installed {
        /// The build the dependency was installed at.
        build: u64,
        /// The dependency's signed `tree_root`.
        tree_root: String,
    },
    /// Already active at `build`.
    AlreadyPresent(u64),
    /// Satisfied by a system install ([`crate::manifest::Program::system`]).
    System(PathBuf),
    /// Skipped, with a reason (unreachable / tombstoned / not pinned / cycle / fetch error).
    Skipped(String),
}

fn index_roster(index: &crate::manifest::Index) -> Vec<String> {
    index.programs.keys().cloned().collect()
}

/// The head of [`FlowError::InsufficientDisk`]'s sentence.
pub const DISK_REFUSAL_HEAD: &str = "not enough disk space";

/// Why an install failed, fail-closed at each gate.
#[derive(Debug)]
pub enum FlowError {
    /// No signature-valid index at/above the floor was found.
    NoIndex,
    /// The index source could not be reached (offline, DNS, a proxy, a 403 rate limit).
    Unreachable(String),
    /// The program is not named in the verified index (unreachable).
    NotReachable(String, Vec<String>),
    /// The requested channel does not exist in the index.
    NoChannel(String),
    /// The program is not pinned in the channel.
    NotPinned(String),
    /// The selected index's freshness window has lapsed (`now >= valid_until`).
    Stale,
    /// The pinned build is yanked/below-floor.
    Tombstoned(String),
    /// Fetching the per-build manifest failed.
    PkgFetch(String),
    /// The per-build manifest did not verify under the delegated release key.
    PkgVerify,
    /// The per-build manifest was malformed / a newer schema.
    PkgParse,
    /// The per-build manifest spells a retired `kind` (`vendor-fetch`).
    RetiredKind(&'static str),
    /// The per-build manifest's `shim_env` breaks the rule a shim can honour.
    ShimEnv(String),
    /// The signed `program`/`build_number` did not match the request (anti-replay).
    Mismatch,
    /// No artifact for the target triple (a clean skip).
    NoArtifact(String),
    /// The artifact's `kind` is not installable by this tool path (fail closed).
    UnsupportedKind(String),
    /// An `app-bundle` member over `github-release`, the aterm app itself.
    AppBundleRefused(String),
    /// An artifact row failed per-protocol admission ([`crate::vendor::check_row`]).
    VendorRefused(String),
    /// The program's signed index row names the retired `requires` relation.
    RequiresRetired(String),
    /// The asset download failed.
    Download(String),
    /// Staging (signed digest / extract / tree_root) failed.
    Stage(StageError),
    /// A recent attempt proved the published asset does not match the pinned build's signed
    /// digests and that refusal still binds ([`crate::store::StageRefusal`]), so nothing
    /// was fetched, staged or changed.
    StageRefused(String),
    /// Activation / shim install failed.
    Activate(String),
    /// Rollback-specific failure.
    Rollback(String),
    /// Disk preflight failed.
    InsufficientDisk { required: u64, available: u64 },
    /// The program is dev-linked (linkmode).
    Linked(String),
    /// The vendor-direct lane did not install a vendor program.
    Vendor(String),
    /// The vendor's release channel was not reached.
    VendorUnreachable(String),
    /// The vendor publishes nothing aterm can verify for this target.
    VendorUnavailable(String),
}

impl std::fmt::Display for FlowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlowError::NoIndex => f.write_str("no signature-valid index at/above the floor"),
            FlowError::Unreachable(why) => {
                // CLI edge appends the per-verb re-run (`print_unreachable_followup`).
                write!(f, "could not reach the toolchain index: {why}")
            }
            FlowError::NotReachable(p, roster) => {
                write!(f, "{p} is not named in the signed index")?;
                match roster.len() {
                    0 => Ok(()),
                    1..=12 => write!(f, " (it names: {})", roster.join(", ")),
                    _ => write!(f, " (it names: {}, …)", roster[..12].join(", ")),
                }
            }
            FlowError::NoChannel(c) => write!(f, "no channel {c:?} in the index"),
            FlowError::NotPinned(p) => write!(f, "{p} is not pinned in the channel"),
            FlowError::Stale => f.write_str("the signed index's freshness window has lapsed"),
            FlowError::Tombstoned(p) => write!(f, "ALab withdrew {p}'s pinned build"),
            FlowError::PkgFetch(e) => write!(f, "fetch manifest: {e}"),
            FlowError::PkgVerify => f.write_str("manifest signature did not verify"),
            FlowError::PkgParse => f.write_str("manifest malformed or newer schema"),
            FlowError::RetiredKind(why) => write!(f, "manifest refused: {why}"),
            FlowError::ShimEnv(why) => write!(f, "manifest refused: {why}"),
            FlowError::Mismatch => f.write_str("manifest program/build did not match the request"),
            FlowError::NoArtifact(t) => write!(f, "no artifact for target {t}"),
            FlowError::UnsupportedKind(k) => write!(
                f,
                "artifact kind {k:?} is not installable by `atpkg install`"
            ),
            FlowError::AppBundleRefused(p) => write!(
                f,
                "{p} updates itself, not through atpkg (`aterm update status`)"
            ),
            FlowError::VendorRefused(why) => write!(f, "artifact row refused: {why}"),
            FlowError::RequiresRetired(p) => write!(
                f,
                "{p}: signed index row names `requires`, which this client no longer honours; \
                 refused"
            ),
            FlowError::Download(e) => write!(f, "download: {e}"),
            FlowError::Stage(e) => write!(f, "stage: {e}"),
            FlowError::StageRefused(m) => f.write_str(m),
            FlowError::Activate(e) => write!(f, "activate: {e}"),
            FlowError::Rollback(m) => {
                // CLI edge already prints the rollback head.
                f.write_str(m)
            }
            FlowError::InsufficientDisk {
                required,
                available,
            } => {
                let (need, have) = crate::cost::human_bytes_apart(*required, *available);
                let short = crate::cost::human_bytes(required.saturating_sub(*available));
                write!(
                    f,
                    "{DISK_REFUSAL_HEAD}: needs {need} free, {have} free here ({short} short)"
                )
            }
            FlowError::Linked(p) => write!(
                f,
                "{p} is dev-linked; run `aterm pkg unlink {p}` to release it"
            ),
            FlowError::Vendor(line)
            | FlowError::VendorUnreachable(line)
            | FlowError::VendorUnavailable(line) => f.write_str(line),
        }
    }
}

impl std::error::Error for FlowError {}

/// What to install: the `channel` to resolve the pin from, the `program`, the target `triple`.
#[derive(Debug, Clone, Copy)]
pub struct InstallRequest<'a> {
    /// The channel whose pin set names the build.
    pub channel: &'a str,
    /// The program to install.
    pub program: &'a str,
    /// The target triple to select an artifact.
    pub triple: &'a str,
    /// The currently-active build, if any (the upgrade/up-to-date input).
    pub installed: Option<u64>,
}

fn reassert_witness(layout: &Layout, program: &str, build: u64) -> Result<(), FlowError> {
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
    activate_build(layout, &build_dir).map_err(|e| FlowError::Activate(e.to_string()))
}

/// Lay the shim of each command the up-to-date `build` exposes whose shim is gone or runs another
/// of `program`'s builds, naming them.
///
/// The commands come from the build's record ([`crate::store::recorded_exposes`]), or else its
/// signed manifest, which then becomes the record. When that manifest cannot be read, it lays
/// nothing while a command of `program` is still on PATH under its own name
/// ([`crate::installed_exposes`]), and otherwise fails with the read's error.
fn lay_commands_off_path(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    repo: &str,
    program: &str,
    build: u64,
) -> Result<Vec<String>, FlowError> {
    let build_dir = layout.build_dir(program, build);
    let tools = match crate::store::recorded_exposes(&build_dir) {
        Some(tools) => tools,
        None => match signed_exposes(fetcher, index, repo, program, build) {
            Ok(tools) => {
                let _ = crate::store::write_exposes(&build_dir, &tools);
                tools
            }
            Err(_) if crate::installed_exposes(layout, program).is_some() => {
                return Ok(Vec::new());
            }
            Err(FlowError::PkgFetch(e)) => {
                note_fetch_fault();
                return Err(FlowError::PkgFetch(format!(
                    "{e}; build {build} keeps no record of the commands it puts on PATH and none \
                     of them is left under its own name, so its signed manifest is needed to put \
                     them back"
                )));
            }
            Err(e) => return Err(e),
        },
    };
    // A shim at another of the program's builds is laid too: a lay prunes every shim of the
    // program it does not lay that runs another build ([`install_tools_env`]).
    let off: Vec<ToolName> = tools
        .into_iter()
        .filter(|tool| {
            let shim = layout.shim(tool);
            match std::fs::symlink_metadata(&shim) {
                Err(e) => e.kind() == std::io::ErrorKind::NotFound,
                Ok(_) => crate::platform::resolve_shim(&shim)
                    .and_then(|target| crate::ops::store_build_of(&layout.prefix, &target))
                    .is_some_and(|(owner, b)| owner == program && b != build),
            }
        })
        .collect();
    if off.is_empty() {
        return Ok(Vec::new());
    }
    install_tools_env(
        layout,
        &build_dir,
        &off,
        Aliases::for_program(program, index.program(program)),
        &crate::shim_env::read_sidecar(&build_dir),
    )
    .map_err(|e| FlowError::Activate(e.to_string()))?;
    Ok(off.iter().map(|tool| tool.as_str().to_string()).collect())
}

/// The admitted `exposes` of `program`'s signed manifest for `build`, read and verified as an
/// install reads it.
fn signed_exposes(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    repo: &str,
    program: &str,
    build: u64,
) -> Result<Vec<ToolName>, FlowError> {
    let (raw, sig) = fetcher
        .pkg_manifest(repo, program, build)
        .map_err(FlowError::PkgFetch)?;
    let verified = index
        .verify_pkg(raw, &sig)
        .map_err(|_| FlowError::PkgVerify)?;
    let pkg = parse_pkg(&verified).map_err(pkg_parse_error)?;
    if !pkg.is_for(program) || pkg.build_number != build {
        return Err(FlowError::Mismatch);
    }
    Ok(crate::store::split_exposed(&pkg.exposes).0)
}

/// The [`FlowError`] a signed manifest that verified but did not parse is refused with.
fn pkg_parse_error(e: crate::Reject) -> FlowError {
    match e {
        crate::Reject::RetiredKind(why) => FlowError::RetiredKind(why),
        crate::Reject::ShimEnv(why) => FlowError::ShimEnv(why),
        _ => FlowError::PkgParse,
    }
}

fn installed_for_decide(layout: &Layout, program: &str, installed: Option<u64>) -> Option<u64> {
    installed.filter(|&build| crate::store::build_is_complete(&layout.build_dir(program, build)))
}

/// Install (or force-upgrade) the program named by `req`, with `fetcher` for all network I/O.
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

/// [`install`], also filling `resolved_assets` with `program → pinned asset file name`.
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

/// [`install`] with a cycle-guard `seen` map threaded through the `requires` recursion.
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
    if crate::linkmode::is_linked(layout, program) {
        return Err(FlowError::Linked(program.to_string()));
    }
    if let Some(spec) = crate::vendor_direct::spec(program)
        && !crate::alab::agent(program).is_some_and(|agent| {
            agent.serves(triple)
                && resolve_verified_index(fetcher, layout, anchor, floor, now_unix)
                    .ok()
                    .is_some_and(|index| {
                        crate::alab::index_owns(layout, program, triple, Some(&index))
                    })
        })
    {
        return vendor_install_outcome(
            fetcher,
            layout,
            anchor,
            floor,
            req,
            spec,
            &crate::vendor_direct::lane::Trust::PRODUCTION,
            now_unix,
        )
        .into_install(layout);
    }
    seen.insert(program.to_string(), None);
    let index = resolve_verified_index(fetcher, layout, anchor, floor, now_unix)?;
    let row = index
        .program(program)
        .ok_or_else(|| FlowError::NotReachable(program.to_string(), index_roster(&index)))?;
    if row.names_requires() {
        return Err(FlowError::RequiresRetired(program.to_string()));
    }
    let repo = &row.repo;
    let ch = index
        .channel_for(channel, triple)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    let ch = &ch;
    let &pinned = ch
        .pin
        .get(program)
        .ok_or_else(|| FlowError::NotPinned(program.to_string()))?;

    match decide(
        ch,
        program,
        installed_for_decide(layout, program, installed),
    ) {
        ApplyDecision::UpToDate => {
            reassert_witness(layout, program, pinned)?;
            let shimmed = lay_commands_off_path(fetcher, layout, &index, repo, program, pinned)?;
            return Ok(InstallReport {
                program: program.to_string(),
                build: pinned,
                index_build: index.index_build,
                roster_seq: index.roster_seq(),
                already_current: true,
                shimmed,
                refused_shims: vec![],
                tree_root: String::new(),
                dependencies: vec![],
                vendor: None,
            });
        }
        ApplyDecision::Tombstone => {
            install_tombstone_shims(layout, program, installed);
            return Err(FlowError::Tombstoned(program.to_string()));
        }
        ApplyDecision::NotPinned => return Err(FlowError::NotPinned(program.to_string())),
        ApplyDecision::Install => {}
    }

    let (raw, sig) = fetcher.pkg_manifest(repo, program, pinned).map_err(|e| {
        note_fetch_fault();
        FlowError::PkgFetch(e)
    })?;
    let verified = index
        .verify_pkg(raw, &sig)
        .map_err(|_| FlowError::PkgVerify)?;
    let pkg = parse_pkg(&verified).map_err(pkg_parse_error)?;
    if !pkg.is_for(program) || pkg.build_number != pinned {
        return Err(FlowError::Mismatch);
    }

    let requires = &pkg.requires;
    let mut dependencies: Vec<DepOutcome> = Vec::new();
    let active = crate::ops::active_builds(layout);
    for dep in requires {
        if let Some(earlier) = seen.get(dep) {
            match earlier {
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
        let system = index
            .program(dep)
            .and_then(|p| p.system.as_deref())
            .and_then(|bin| {
                crate::vendor::system_binary_on_path(
                    &layout.prefix,
                    bin,
                    crate::vendor::path_var().as_deref(),
                )
            });
        if let Some(path) = system {
            dependencies.push(DepOutcome {
                program: dep.clone(),
                result: DepResult::System(path),
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
                DepResult::Installed {
                    build: r.build,
                    tree_root: r.tree_root,
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

    let artifact = pkg
        .artifact_for(triple)
        .ok_or_else(|| FlowError::NoArtifact(triple.to_string()))?;
    resolved_assets.insert(program.to_string(), artifact.asset.clone());
    crate::vendor::check_row(artifact, &pkg.exposes)?;
    let strategy = crate::dispatch::strategy_for(&artifact.kind, &artifact.protocol);
    match strategy {
        crate::dispatch::ApplyStrategy::Shim
        | crate::dispatch::ApplyStrategy::SysrootBundle
        | crate::dispatch::ApplyStrategy::VendorApp => {}
        crate::dispatch::ApplyStrategy::AppBundle => {
            return Err(FlowError::AppBundleRefused(program.to_string()));
        }
        crate::dispatch::ApplyStrategy::Unknown => {
            return Err(FlowError::UnsupportedKind(artifact.kind.clone()));
        }
    }

    let shim_env = pkg.shim_env();
    let alab_gate = alab_pack_gate(program, &pkg.exposes);
    let hooks = stage_hooks(alab_gate.as_deref());
    let landed = land_artifact(
        layout,
        &Landing {
            program,
            build: pinned,
            artifact,
            exposes: &pkg.exposes,
            shim_env: &shim_env,
            aliases: Aliases::for_program(program, index.program(program)),
            installed,
            strategy,
            hooks: &hooks,
            download_turn: None,
        },
        &|dl| fetch_artifact(fetcher, program, repo, artifact, dl),
    )?;
    let _ = crate::status::record_refused_shims(layout, program, pinned, &landed.refused);
    Ok(InstallReport {
        program: program.to_string(),
        build: pinned,
        index_build: index.index_build,
        roster_seq: index.roster_seq(),
        already_current: false,
        shimmed: landed.shimmed,
        refused_shims: landed.refused,
        tree_root: artifact.tree_root.clone(),
        dependencies,
        vendor: None,
    })
}

/// An install of a vendor-direct program (`req.program`, row `spec`).
#[allow(
    clippy::too_many_arguments,
    reason = "the install request plus the index gate the door's yank refresh verifies under"
)]
pub(crate) fn vendor_install_outcome(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    floor: BuildFloor,
    req: &InstallRequest,
    spec: &'static crate::vendor_direct::VendorSpec,
    trust: &crate::vendor_direct::lane::Trust,
    now_unix: i64,
) -> crate::vendor_direct::lane::Outcome {
    let policy = if spec.serves(req.triple) {
        door_vendor_policy(fetcher, layout, anchor, floor, now_unix)
    } else {
        crate::vendor_direct::policy::Policy::default()
    };
    let lane = crate::vendor_direct::lane::Lane {
        layout,
        fetcher,
        triple: req.triple,
        policy: &policy,
        trust,
        legacy_index: None,
        asked: true,
        now: now_unix,
        download_turn: None,
    };
    crate::vendor_direct::lane::install_one(&lane, spec, false)
}

/// The signed yanks a targeted vendor door decides under.
pub(crate) fn door_vendor_policy(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    floor: BuildFloor,
    now_unix: i64,
) -> crate::vendor_direct::policy::Policy {
    use crate::vendor_direct::policy::{Policy, YankLatch};
    if let Some(latch) = YankLatch::read(layout)
        && latch.is_current(now_unix)
    {
        return latch.policy;
    }
    let fresh = verify_select_fresh(fetcher, layout, anchor, floor, now_unix);
    Policy::current(layout, fresh.as_ref().ok())
}

/// One store landing: the admitted row, the build it becomes, and what its shims carry.
pub(crate) struct Landing<'a> {
    /// The program.
    pub program: &'a str,
    /// The store build the row becomes.
    pub build: u64,
    /// The admitted row.
    pub artifact: &'a crate::manifest::Artifact,
    /// The tool names the build exposes (admitted here).
    pub exposes: &'a [String],
    /// What the shims export, recorded beside the build before it is staged.
    pub shim_env: &'a crate::shim_env::ShimEnv,
    /// Whether the tools get `alab-<tool>` aliases.
    pub aliases: Aliases,
    /// The active build the caller decided against.
    pub installed: Option<u64>,
    /// How the row applies ([`crate::dispatch::strategy_for`]).
    pub strategy: crate::dispatch::ApplyStrategy,
    /// The caller's gate and sidecars inside the stage.
    pub hooks: &'a crate::install::StageHooks<'a>,
    /// This landing's place in line for the link among landings side by side.
    pub download_turn: Option<&'a DownloadTicket<'a>>,
}

/// The order landings side by side take the link in: one transfer at a time, in the order their
/// programs were taken ([`Self::ticket`]), not the order their threads reach the download. The
/// first program taken has the whole link; a later landing waits at its transfer, and each
/// landing's verify, unpack and activate overlap the next one's transfer.
#[derive(Default)]
pub(crate) struct DownloadTurns {
    state: std::sync::Mutex<Turns>,
    changed: std::sync::Condvar,
}

#[derive(Default)]
struct Turns {
    next: u64,
    open: std::collections::BTreeSet<u64>,
}

impl DownloadTurns {
    /// The next place in line, behind every ticket taken before it.
    pub(crate) fn ticket(&self) -> DownloadTicket<'_> {
        let mut turns = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let n = turns.next;
        turns.next = n.saturating_add(1);
        turns.open.insert(n);
        DownloadTicket { turns: self, n }
    }
}

/// One landing's place in line ([`DownloadTurns::ticket`]).
pub(crate) struct DownloadTicket<'t> {
    turns: &'t DownloadTurns,
    n: u64,
}

impl DownloadTicket<'_> {
    /// Wait until every ticket taken before this one is done with the link.
    pub(crate) fn wait(&self) {
        let turns = self
            .turns
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(
            self.turns
                .changed
                .wait_while(turns, |turns| !is_next(turns, self.n))
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }

    /// Whether this ticket's transfer may run now: every ticket taken before it is done.
    #[cfg(test)]
    pub(crate) fn holds_the_link(&self) -> bool {
        is_next(
            &self
                .turns
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            self.n,
        )
    }

    /// Done with the link: the next ticket's transfer may start; releasing twice is releasing once.
    pub(crate) fn release(&self) {
        self.turns
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .open
            .remove(&self.n);
        self.turns.changed.notify_all();
    }
}

impl Drop for DownloadTicket<'_> {
    fn drop(&mut self) {
        self.release();
    }
}

fn is_next(turns: &Turns, n: u64) -> bool {
    turns.open.first() == Some(&n)
}

struct TurnHeld<'a>(Option<&'a DownloadTicket<'a>>);

impl Drop for TurnHeld<'_> {
    fn drop(&mut self) {
        if let Some(ticket) = self.0 {
            ticket.release();
        }
    }
}

/// What a landing laid.
pub(crate) struct Landed {
    /// The tools that got a shim.
    pub shimmed: Vec<String>,
    /// The exposed names refused a shim.
    pub refused: Vec<String>,
    /// The `tree_root` the stage folded.
    pub root: String,
}

/// Fetch the row's bytes with `fetch`, verify and stage them, activate the build and lay its shims.
pub(crate) fn land_artifact(
    layout: &Layout,
    l: &Landing<'_>,
    fetch: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<Landed, FlowError> {
    let (program, pinned, artifact, installed, strategy) =
        (l.program, l.build, l.artifact, l.installed, l.strategy);
    let turn = TurnHeld(l.download_turn);
    if let Some(note) = digest_refusal_note(layout, program, pinned, artifact, false) {
        return Err(FlowError::StageRefused(note));
    }
    let dl = staged_download_path(layout, program, &artifact.asset)?;
    let ahead = ahead::claim(program, &dl);
    if ahead.is_none() {
        let required = LandingBytes::of(artifact).need();
        disk_gate(required, crate::freespace::available_bytes(&dl))?;
    }
    make_staging_dir(&dl).map_err(|e| FlowError::Download(e.to_string()))?;
    let reusable = ahead.is_none() && carried_archive(&dl, artifact);
    if ahead.is_none() && !reusable {
        let _ = std::fs::remove_file(&dl);
    }
    if ahead.is_none() {
        sweep_foreign_partials(&dl);
    }
    crate::progress::note_build(program, pinned);
    if let Some(ticket) = turn.0 {
        if ahead.is_none() && !reusable {
            ticket.wait();
        } else {
            ticket.release();
        }
    }
    let download_watch = ahead
        .is_none()
        .then(|| crate::progress::watch_download(program, &dl, artifact.size));
    if reusable {
        println!(
            "atpkg: {program}: reusing {}, already downloaded and matching its digest",
            artifact.asset
        );
    } else {
        let inline = ahead.is_none();
        if let Err(e) = ahead.unwrap_or_else(|| fetch(&dl)) {
            let _ = std::fs::remove_file(&dl);
            note_fetch_fault();
            return Err(FlowError::Download(e));
        }
        if inline {
            ahead::fetched_inline(program, &dl);
        }
    }
    drop(download_watch);
    drop(turn);
    crate::progress::note_phase(program, crate::progress::Phase::Verify);
    let build_dir = layout.build_dir(program, pinned);
    let was_live =
        std::fs::read_link(layout.program_current(program)).is_ok_and(|t| t == build_dir);
    if let Err(e) = disk_gate(
        artifact.cost.disk_installed,
        crate::freespace::available_bytes(&build_dir),
    ) {
        let _ = std::fs::remove_file(&dl);
        return Err(e);
    }
    if let Err(e) = crate::shim_env::write_sidecar(&build_dir, l.shim_env) {
        let _ = std::fs::remove_file(&dl);
        let mut why = String::from("shim_env sidecar: ");
        why.push_str(&e.to_string());
        return Err(FlowError::Activate(why));
    }
    let extract_scope = crate::progress::extract_scope(program, artifact.cost.disk_installed);
    let staged = verify_and_stage(artifact, &dl, &build_dir, l.hooks);
    drop(extract_scope);
    let root = match staged {
        Ok(root) => root,
        Err(e) => {
            record_digest_refusal(&build_dir, artifact, &e);
            reclaim_after_failed_stage(&dl, &e);
            return Err(FlowError::Stage(e));
        }
    };
    let _ = std::fs::remove_file(&dl);
    crate::store::clear_stage_refusal(&build_dir);

    if strategy == crate::dispatch::ApplyStrategy::SysrootBundle {
        apply_sysroot_bundle(&artifact.reloc)?;
    }

    crate::progress::note_phase(program, crate::progress::Phase::Link);
    activate_build(layout, &build_dir).map_err(|e| FlowError::Activate(e.to_string()))?;
    let (tools, refused) = crate::store::split_exposed(l.exposes);
    // What an up-to-date install and `repair` re-lay from; a build without it falls back.
    let _ = crate::store::write_exposes(&build_dir, &tools);
    let aliases = l.aliases;
    let staged = Staged {
        build: pinned,
        build_dir,
        exposes: tools,
        prior_build: installed,
        was_live,
        reloc: None,
        tree_root: String::new(),
        aliases,
    };
    let (build_dir, tools) = (&staged.build_dir, &staged.exposes);
    if let Err(e) = install_tools_env(layout, build_dir, tools, aliases, l.shim_env) {
        abort_activated_install(layout, program, &staged);
        return Err(FlowError::Activate(e.to_string()));
    }

    if strategy == crate::dispatch::ApplyStrategy::SysrootBundle
        && let Err(e) = bundle_resolve_check(build_dir, tools)
    {
        abort_activated_install(layout, program, &staged);
        return Err(e);
    }
    let shimmed: Vec<String> = tools.iter().map(|t| t.as_str().to_string()).collect();
    Ok(Landed {
        shimmed,
        refused,
        root,
    })
}

fn fetch_artifact(
    fetcher: &dyn Fetcher,
    program: &str,
    repo: &str,
    artifact: &crate::manifest::Artifact,
    dl: &Path,
) -> Result<(), String> {
    match artifact.protocol.as_str() {
        "github-release" => fetcher.download_for(program, repo, &artifact.asset, dl, artifact.size),
        "https" => fetcher.download_url(&artifact.url, dl, artifact.size),
        other => {
            let mut m = String::from("protocol ");
            m.push_str(other);
            m.push_str(" has no download lane in this client");
            Err(m)
        }
    }
}

/// Re-point `program` at its retained build `to` (the vendor lane's signed-yank rollback).
pub(crate) fn roll_back_to(
    layout: &Layout,
    program: &str,
    to: u64,
    exposes: &[String],
) -> std::io::Result<()> {
    let build_dir = layout.build_dir(program, to);
    activate_build(layout, &build_dir)?;
    let (tools, _) = crate::store::split_exposed(exposes);
    let env = crate::shim_env::read_sidecar(&build_dir);
    install_tools_env(layout, &build_dir, &tools, Aliases::Off, &env)
}

/// Lay a failing tombstone shim over every tool `program`'s active build exposes.
pub(crate) fn install_tombstone_shims(layout: &Layout, program: &str, installed: Option<u64>) {
    if let Some(cur) = installed {
        let tools = crate::ops::active_tools(layout, program, cur);
        let aliases = crate::ops::active_aliases(layout, program, cur);
        for tool in tools.iter().chain(&aliases) {
            let _ = install_tombstone_shim(layout, tool);
        }
        crate::activate::remove_refused_name_shims(layout, Some(program));
    }
}

fn abort_activated_install(layout: &Layout, program: &str, staged: &Staged) {
    rollback_member(layout, program, staged);
    crate::activate::undo_activation(layout, &staged.build_dir);
    if staged.was_live {
        // Re-point the witness at the kept tree, or the keep is one-shot. Exists, not
        // resolves, is the claim test (as in `gc::authority_claims`), so a link a
        // prior-build rollback wrote is never overwritten.
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

struct Staged {
    build: u64,
    build_dir: PathBuf,
    /// The admitted tools, not the raw manifest list.
    exposes: Vec<ToolName>,
    /// The member's prior active build (`None` ⇒ a fresh install: rollback removes the shims).
    prior_build: Option<u64>,
    /// Whether `build_dir` was already the live build when staged.
    was_live: bool,
    /// `Some(reloc_policy)` for a `sysroot-bundle` member.
    reloc: Option<String>,
    /// The signed `tree_root` of the staged build.
    tree_root: String,
    /// Whether the member's tools get `alab-<tool>` aliases.
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
    /// Per member flipped live, keyed by program.
    pub applied: BTreeMap<String, AppliedMember>,
    /// Members excluded from this apply because they are dev-linked.
    pub skipped_linked: Vec<String>,
    /// `program → pinned artifact asset file name` for every member resolved far enough to
    /// select an artifact, including members whose download or stage then failed: the
    /// pass-end [`crate::gc::run_keeping_pinned_partials`] spares exactly these programs'
    /// `.part` resume files.
    pub resolved_assets: BTreeMap<String, String>,
}

/// A member the apply flipped live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMember {
    /// The build now active.
    pub build: u64,
    /// The signed `tree_root` of that build.
    pub tree_root: String,
}

/// What a [`rollback`] did.
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
    /// The local pins that keep it on `to_build` through the next update.
    pub held_by: Vec<String>,
    /// A member of its group whose current build is revoked (yanked or below the floor).
    pub revoked_member: Option<(String, u64)>,
}

/// Update every program the channel pins, group by group.
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
    let index = resolve_verified_index(fetcher, layout, anchor, floor, now_unix)?;
    apply_channel_gated(
        fetcher,
        layout,
        &index,
        channel,
        triple,
        installed,
        excluded,
        &crate::quiet::FlipGate::NOW,
    )
}

/// [`apply_channel`] over a caller-resolved, verified index, under `flip`.
#[allow(
    clippy::too_many_arguments,
    reason = "apply_channel's inputs, with the resolved index in place of its resolve \
              inputs, plus the flip gate the unattended lanes hand in"
)]
pub fn apply_channel_gated(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    channel: &str,
    triple: &str,
    installed: &BTreeMap<String, u64>,
    excluded: &[String],
    flip: &crate::quiet::FlipGate<'_>,
) -> Result<ChannelApplyReport, FlowError> {
    let ch = index
        .channel_for(channel, triple)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;

    let report = apply_groups(
        fetcher,
        layout,
        index,
        &ch,
        triple,
        &crate::apply::plan_groups_here(layout, index, &ch, triple),
        installed,
        excluded,
        flip,
    );
    if !report.groups.iter().any(|(g, o)| {
        g.members.iter().any(|m| m == crate::seam::SEAM_PROGRAM)
            && matches!(o, TxnOutcome::Deferred { .. } | TxnOutcome::Aborted { .. })
    }) {
        crate::quiet::clear(layout);
    }
    Ok(report)
}

/// Apply each of `groups` as its own all-or-nothing transaction ([`apply_group`]).
#[allow(
    clippy::too_many_arguments,
    reason = "apply_group's inputs, over a slice of groups"
)]
fn apply_groups(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    ch: &Channel,
    triple: &str,
    groups: &[Group],
    installed: &BTreeMap<String, u64>,
    excluded: &[String],
    flip: &crate::quiet::FlipGate<'_>,
) -> ChannelApplyReport {
    let mut report = ChannelApplyReport {
        index_build: index.index_build,
        roster_seq: index.roster_seq(),
        groups: Vec::with_capacity(groups.len()),
        applied: BTreeMap::new(),
        skipped_linked: Vec::new(),
        resolved_assets: BTreeMap::new(),
    };
    for group in groups {
        let linked: Vec<String> = group
            .members
            .iter()
            .filter(|m| crate::linkmode::is_linked(layout, m))
            .cloned()
            .collect();
        if !linked.is_empty() {
            report.skipped_linked.extend(linked);
            continue;
        }
        if let Some((acted, outcome, group_applied)) = apply_group(
            fetcher,
            layout,
            index,
            ch,
            triple,
            group,
            installed,
            excluded,
            &mut report.resolved_assets,
            flip,
        ) {
            report.applied.extend(group_applied);
            report.groups.push((acted, outcome));
        }
    }
    report
}

/// Whether any of `programs` is installed here but disabled.
pub(crate) fn tombstoned_in_place<'a>(
    layout: &Layout,
    programs: impl IntoIterator<Item = &'a str>,
) -> bool {
    let Ok(entries) = std::fs::read_dir(layout.bin_dir()) else {
        return false;
    };
    let tombstones: Vec<ToolName> = entries
        .flatten()
        .filter(|e| crate::platform::resolve_shim(&e.path()).is_none())
        .filter_map(|e| ToolName::from_shim_file(e.file_name().to_str()?))
        .collect();
    if tombstones.is_empty() {
        return false;
    }
    let live = crate::gc::live_builds(layout);
    programs.into_iter().any(|m| {
        live.get(m).is_some_and(|witness| {
            let build_bin = layout.build_dir(m, witness.build()).join("bin");
            tombstones
                .iter()
                .any(|tool| !crate::store::presence(&build_bin.join(tool.exe_file())).is_absent())
        })
    })
}

fn deliberately_absent<'a>(
    layout: &Layout,
    installed: &'a BTreeMap<String, u64>,
    excluded: &'a [String],
) -> impl Fn(&String) -> bool + use<'a> {
    let removed = layout.removed_programs();
    let declined = layout.declined().is_file();
    move |m: &String| {
        (declined || removed.contains(m) || excluded.contains(m)) && !installed.contains_key(m)
    }
}

/// The members whose local pins the update pass's group gate reads for `program`.
pub(crate) fn pin_gate_members(
    layout: &Layout,
    index: &Index,
    ch: &Channel,
    program: &str,
    active: &BTreeMap<String, u64>,
    excluded: &[String],
) -> Vec<String> {
    let absent = deliberately_absent(layout, active, excluded);
    crate::apply::plan_groups_here(layout, index, ch, crate::cli::current_triple())
        .into_iter()
        .find(|g| g.members.iter().any(|m| m == program))
        .map_or_else(|| vec![program.to_string()], |g| g.members)
        .into_iter()
        .filter(|m| !absent(m))
        .collect()
}

/// Apply one coherence group as an all-or-nothing transaction.
#[allow(
    clippy::too_many_arguments,
    reason = "the per-group apply needs the same irreducible inputs as apply_channel: the \
              network fetcher, the layout, the verified index + channel, the triple \
              selector, the group, the installed-build map, and the caller's \
              resolved-asset collector the pass-end gc sparing reads"
)]
fn apply_group(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    ch: &Channel,
    triple: &str,
    group: &Group,
    installed: &BTreeMap<String, u64>,
    excluded: &[String],
    resolved_assets: &mut BTreeMap<String, String>,
    flip: &crate::quiet::FlipGate<'_>,
) -> Option<(Group, TxnOutcome, BTreeMap<String, AppliedMember>)> {
    // With no member installed, the group applies only when one is tombstoned in place.
    // `installed`, the map `decide` reads, is not widened for that (it would answer UpToDate for
    // an unmoved pin), so `decide` returns Install, the re-stage [`Staged::was_live`] exists for.
    if group.members.iter().all(|m| !installed.contains_key(m))
        && !tombstoned_in_place(layout, group.members.iter().map(String::as_str))
    {
        return None;
    }
    let absent = deliberately_absent(layout, installed, excluded);
    let acted = Group {
        group: group.group.clone(),
        members: group
            .members
            .iter()
            .filter(|m| !absent(m))
            .cloned()
            .collect(),
    };
    if acted.members.is_empty() {
        return None;
    }
    let (outcome, applied) = apply_group_txn(
        fetcher,
        layout,
        index,
        ch,
        triple,
        &acted,
        installed,
        resolved_assets,
        flip,
    );
    Some((acted, outcome, applied))
}

/// Bootstrap: apply one coherence group all-or-nothing against a caller-resolved, verified index.
#[allow(
    clippy::too_many_arguments,
    reason = "the bootstrap group apply needs the same irreducible inputs as apply_group \
              minus the pre-resolved channel it looks up itself: fetcher, layout, verified \
              index, channel + triple selectors, the group, the installed-build map, the \
              caller's resolved-asset collector the pass-end gc sparing reads, and the \
              caller's flip gate"
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
    flip: &crate::quiet::FlipGate<'_>,
) -> Result<(TxnOutcome, BTreeMap<String, AppliedMember>), FlowError> {
    let ch = index
        .channel_for(channel, triple)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    Ok(apply_group_txn(
        fetcher,
        layout,
        index,
        &ch,
        triple,
        group,
        installed,
        resolved_assets,
        flip,
    ))
}

/// Bootstrap prescan: `Some(member)` iff one of `members` has no artifact for `triple`.
pub fn group_missing_triple(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    channel: &str,
    triple: &str,
    members: &[String],
) -> Option<String> {
    group_missing_triple_reading(fetcher, index, channel, triple, members, &mut |_, _| {})
}

/// [`group_missing_triple`].
pub(crate) fn group_missing_triple_reading(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    channel: &str,
    triple: &str,
    members: &[String],
    read: &mut dyn FnMut(&str, &crate::manifest::PkgManifest),
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
        read(m, &pkg);
    }
    None
}

/// The transaction body shared by [`apply_group`] (update: installed groups only).
#[allow(
    clippy::too_many_arguments,
    reason = "the per-group apply needs the same irreducible inputs as apply_channel: the \
              network fetcher, the layout, the verified index + channel, the triple \
              selector, the group, the installed-build map, and the caller's \
              resolved-asset collector the pass-end gc sparing reads"
)]
fn apply_group_txn(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    ch: &Channel,
    triple: &str,
    group: &Group,
    installed: &BTreeMap<String, u64>,
    resolved_assets: &mut BTreeMap<String, String>,
    flip: &crate::quiet::FlipGate<'_>,
) -> (TxnOutcome, BTreeMap<String, AppliedMember>) {
    let (outcome, applied) = apply_group_txn_inner(
        fetcher,
        layout,
        index,
        ch,
        triple,
        group,
        installed,
        resolved_assets,
        flip,
    );
    if group.members.iter().any(|m| m == crate::seam::SEAM_PROGRAM)
        && !matches!(
            outcome,
            TxnOutcome::Deferred { .. } | TxnOutcome::Aborted { .. }
        )
    {
        crate::quiet::clear(layout);
    }
    (outcome, applied)
}

struct GroupDecisions {
    /// Per member, in member order.
    decisions: Vec<(String, ApplyDecision)>,
    /// Some member's pin is revoked.
    any_tombstone: bool,
    /// Some Install member moves to a different build, not only a re-stage at its pin.
    wants_upgrade: bool,
    /// Every installed member's current build is still gate-valid.
    all_current_valid: bool,
}

impl GroupDecisions {
    fn of(layout: &Layout, ch: &Channel, group: &Group, installed: &BTreeMap<String, u64>) -> Self {
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
        let wants_upgrade = decisions
            .iter()
            .any(|(m, d)| *d == ApplyDecision::Install && installed.get(m) != ch.pin.get(m));
        let all_current_valid = group
            .members
            .iter()
            .all(|m| crate::gate::current_build_ok(ch, m, installed.get(m).copied()));
        Self {
            decisions,
            any_tombstone,
            wants_upgrade,
            all_current_valid,
        }
    }

    fn install_members(&self) -> Vec<&String> {
        self.decisions
            .iter()
            .filter(|(_, d)| *d == ApplyDecision::Install)
            .map(|(n, _)| n)
            .collect()
    }

    fn retired_requires(&self, index: &TrustedIndex) -> Option<&String> {
        if self.any_tombstone
            || !self.all_current_valid
            || !self
                .decisions
                .iter()
                .any(|(_, d)| *d == ApplyDecision::Install)
        {
            return None;
        }
        self.decisions.iter().map(|(m, _)| m).find(|m| {
            index
                .program(m)
                .is_some_and(crate::manifest::Program::names_requires)
        })
    }

    fn pin_hold(&self, layout: &Layout) -> Option<Vec<String>> {
        if self.any_tombstone || !self.wants_upgrade {
            return None;
        }
        let held: Vec<String> = self
            .decisions
            .iter()
            .map(|(m, _)| m)
            .filter(|m| crate::pin::is_pinned(layout, m))
            .cloned()
            .collect();
        (!held.is_empty() && self.all_current_valid).then_some(held)
    }
}

/// [`apply_group_txn`]'s body.
#[allow(
    clippy::too_many_arguments,
    reason = "apply_group_txn's inputs, passed through whole"
)]
fn apply_group_txn_inner(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    ch: &Channel,
    triple: &str,
    group: &Group,
    installed: &BTreeMap<String, u64>,
    resolved_assets: &mut BTreeMap<String, String>,
    flip: &crate::quiet::FlipGate<'_>,
) -> (TxnOutcome, BTreeMap<String, AppliedMember>) {
    let decided = GroupDecisions::of(layout, ch, group, installed);

    if let Some(member) = decided.retired_requires(index) {
        return (
            TxnOutcome::Aborted {
                failed: member.clone(),
                during_flip: false,
                why: FlowError::RequiresRetired(member.clone()).to_string(),
            },
            BTreeMap::new(),
        );
    }

    if let Some(held) = decided.pin_hold(layout) {
        return (TxnOutcome::Pinned(held), BTreeMap::new());
    }
    let decisions = &decided.decisions;
    let (any_tombstone, all_current_valid) = (decided.any_tombstone, decided.all_current_valid);

    let install_members = decided.install_members();
    let need = if install_members.is_empty() {
        InstallNeed::Unknown
    } else {
        group_install_need(fetcher, index, ch, &install_members, triple)
    };

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
        let disabled = disable_revoked_currents(layout, ch, decisions, installed);
        for program in &disabled {
            println!(
                "{}",
                recalled_unpublished_notice(program, member, *build, triple)
            );
        }
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

    if let InstallNeed::Published { bytes, .. } = &need
        && let Err(short) = disk_gate(
            bytes.need(),
            crate::freespace::available_bytes(&layout.prefix),
        )
    {
        let disabled = disable_revoked_currents(layout, ch, decisions, installed);
        if let FlowError::InsufficientDisk {
            required: need,
            available: have,
        } = &short
        {
            let (need_words, have_words) = crate::cost::human_bytes_apart(*need, *have);
            for program in &disabled {
                println!(
                    "atpkg: {program} was recalled and its replacement needs {need_words} free \
                     ({have_words} free here) \u{2014} its commands are disabled until you free \
                     {} and run `aterm pkg update`",
                    crate::cost::human_bytes(need.saturating_sub(*have))
                );
            }
        }
        return (
            TxnOutcome::Aborted {
                failed: install_members[0].clone(),
                during_flip: false,
                why: short.to_string(),
            },
            BTreeMap::new(),
        );
    }

    let quiet = QuietGate::new(ch, group, installed, &install_members, flip);
    let forced = any_tombstone || !all_current_valid;
    let mut at_ceiling = false;
    let mut unrecorded = false;
    if let Some(q) = &quiet {
        let busy = crate::quiet::probe(layout, &q.live, flip.running);
        match crate::quiet::decide(
            flip.policy,
            forced,
            busy.is_quiet(),
            crate::quiet::since_of(layout, &q.label),
            flip.now,
        ) {
            crate::quiet::Verdict::Flip => {}
            crate::quiet::Verdict::FlipAtCeiling { since } => {
                at_ceiling = true;
                println!(
                    "atpkg: installing the Trust toolchain update ({}) after waiting {}, \
                     though {}; a command already running finishes on the old build",
                    q.moves_words(),
                    waited_words(flip.now.saturating_sub(since)),
                    busy.clause()
                );
            }
            crate::quiet::Verdict::Defer { since } => {
                for m in &install_members {
                    let mut why = String::new();
                    let Some(fetched) = fetch_member(
                        fetcher,
                        layout,
                        index,
                        ch,
                        m,
                        triple,
                        resolved_assets,
                        &mut why,
                    ) else {
                        return (
                            TxnOutcome::Aborted {
                                failed: (*m).clone(),
                                during_flip: false,
                                why,
                            },
                            BTreeMap::new(),
                        );
                    };
                    if !fetched.carried
                        && let Err(err) =
                            crate::install::check_signed_digest(&fetched.artifact, &fetched.dl)
                    {
                        record_digest_refusal(
                            &layout.build_dir(m, fetched.pinned),
                            &fetched.artifact,
                            &err,
                        );
                        reclaim_after_failed_stage(&fetched.dl, &err);
                        eprintln!(
                            "{}",
                            stage_failure_note(m, fetched.pinned, &err.to_string())
                        );
                        return (
                            TxnOutcome::Aborted {
                                failed: (*m).clone(),
                                during_flip: false,
                                why: format!("staging {} failed: {err}", fetched.artifact.asset),
                            },
                            BTreeMap::new(),
                        );
                    }
                }
                if crate::quiet::note_deferred(
                    layout,
                    &q.label,
                    since,
                    flip.now,
                    &busy,
                    &q.moves(resolved_assets),
                ) {
                    return (
                        TxnOutcome::Deferred {
                            members: install_members.iter().map(|m| (*m).clone()).collect(),
                            why: busy.clause(),
                            since,
                        },
                        BTreeMap::new(),
                    );
                }
                unrecorded = true;
                println!(
                    "atpkg: the Trust toolchain update ({}) is installed now, though {}: its \
                     wait could not be recorded in {}; a command already running finishes on \
                     the old build",
                    q.moves_words(),
                    busy.clause(),
                    crate::quiet::record_path(layout).display()
                );
            }
        }
    }

    let staged: RefCell<BTreeMap<String, Staged>> = RefCell::new(BTreeMap::new());
    let assets: RefCell<BTreeMap<String, String>> = RefCell::new(std::mem::take(resolved_assets));
    let ahead: RefCell<BTreeMap<String, ResolveAhead>> = RefCell::new(BTreeMap::new());
    let outcome = transact_holding(
        decisions,
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
                &mut assets.borrow_mut(),
                &mut why,
            ) {
                Some(s) => {
                    if let Some(check) = ResolveAhead::start(&s) {
                        ahead.borrow_mut().insert(m.to_string(), check);
                    }
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
        &mut || {
            let q = quiet.as_ref().filter(|_| !at_ceiling && !unrecorded)?;
            let busy = crate::quiet::probe(layout, &q.live, flip.running);
            let crate::quiet::Verdict::Defer { since } = crate::quiet::decide(
                flip.policy,
                forced,
                busy.is_quiet(),
                crate::quiet::since_of(layout, &q.label),
                flip.now,
            ) else {
                return None;
            };
            crate::quiet::note_deferred(
                layout,
                &q.label,
                since,
                flip.now,
                &busy,
                &q.moves(&assets.borrow()),
            )
            .then(|| (busy.clause(), since))
        },
        &mut |m| {
            crate::progress::note_phase(m, crate::progress::Phase::Link);
            let check = ahead.borrow_mut().remove(m);
            staged
                .borrow()
                .get(m)
                .is_some_and(|s| flip_member(layout, m, s, check))
        },
        &mut |m| {
            if let Some(s) = staged.borrow().get(m) {
                rollback_member(layout, m, s);
            }
        },
    );
    *resolved_assets = assets.into_inner();
    drop(ahead);
    if matches!(
        outcome,
        TxnOutcome::Aborted { .. } | TxnOutcome::Deferred { .. }
    ) {
        for s in staged.borrow().values() {
            // Never delete a build that was already live when re-staged: gc protects it
            // ([`crate::gc::live_builds`]), and deleting it would leave `current` dangling.
            if s.was_live {
                continue;
            }
            crate::store::discard_build(&s.build_dir);
        }
    } else {
        for program in staged.borrow().keys() {
            if let Some(asset) = resolved_assets.get(program)
                && let Ok(dl) = staged_download_path(layout, program, asset)
            {
                let _ = std::fs::remove_file(&dl);
            }
        }
    }
    if let TxnOutcome::Tombstoned(members) = &outcome {
        for m in members {
            install_tombstone_shims(layout, m, installed.get(m).copied());
        }
    }
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

struct QuietGate {
    /// The group's name, the record's key.
    label: String,
    /// Member → the live build the flip would move off (what is probed).
    live: BTreeMap<String, u64>,
    /// Member → the build it would move onto.
    to: BTreeMap<String, u64>,
}

impl QuietGate {
    fn new(
        ch: &Channel,
        group: &Group,
        installed: &BTreeMap<String, u64>,
        install_members: &[&String],
        flip: &crate::quiet::FlipGate<'_>,
    ) -> Option<Self> {
        let applies = flip.policy == crate::quiet::FlipPolicy::WhenQuiet
            && install_members
                .iter()
                .any(|m| *m == crate::seam::SEAM_PROGRAM)
            && installed.contains_key(crate::seam::SEAM_PROGRAM);
        applies.then(|| Self {
            label: group
                .group
                .clone()
                .unwrap_or_else(|| group.members.join("+")),
            live: group
                .members
                .iter()
                .filter_map(|m| installed.get(m).map(|b| (m.clone(), *b)))
                .collect(),
            to: install_members
                .iter()
                .filter_map(|m| ch.pin.get(*m).map(|b| ((*m).clone(), *b)))
                .collect(),
        })
    }

    fn moves_words(&self) -> String {
        crate::quiet::Deferral {
            from: self.live.clone(),
            to: self.to.clone(),
            ..Default::default()
        }
        .moves()
    }

    fn moves(&self, resolved: &BTreeMap<String, String>) -> crate::quiet::Moves {
        crate::quiet::Moves {
            from: self.live.clone(),
            to: self.to.clone(),
            assets: self
                .to
                .keys()
                .filter_map(|m| resolved.get(m).map(|asset| (m.clone(), asset.clone())))
                .collect(),
        }
    }
}

fn waited_words(secs: i64) -> String {
    let mins = secs.max(0) / 60;
    match (mins / 60, mins % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

fn carried_archive(dl: &Path, artifact: &crate::manifest::Artifact) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(dl) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.nlink() != 1 {
            return false;
        }
    }
    crate::install::check_signed_digest(artifact, dl).is_ok()
}

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

/// The line the missing-triple hold prints for each `program` it disables.
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

fn discard_sibling_partial(dl: &Path) {
    let Some(name) = dl.file_name() else {
        return;
    };
    let mut part = name.to_os_string();
    part.push(".part");
    let _ = std::fs::remove_file(dl.with_file_name(part));
}

pub(crate) fn sweep_foreign_partials(dl: &Path) {
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
        let is_archive = entry_name.to_str().is_some_and(|n| {
            n.ends_with(".tar.zst")
                || n.ends_with(".dmg")
                || crate::vendor_direct::VENDORS.iter().any(|spec| {
                    n.strip_prefix(spec.program)
                        .is_some_and(|rest| rest.starts_with('-'))
                })
        });
        if is_part || is_archive {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn reclaim_after_failed_stage(dl: &Path, e: &StageError) {
    if matches!(
        e,
        StageError::DigestMismatch { .. } | StageError::SignerRefused(_)
    ) {
        let _ = std::fs::remove_file(dl);
        discard_sibling_partial(dl);
    }
}

fn record_digest_refusal(build_dir: &Path, artifact: &crate::manifest::Artifact, e: &StageError) {
    let Some(digest) = artifact.digest() else {
        return;
    };
    let record = match e {
        StageError::DigestMismatch { .. } => crate::store::record_stage_refusal,
        StageError::SignerRefused(_) => crate::store::record_signer_refusal,
        StageError::TreeRootMismatch { .. } => crate::store::record_held_refusal,
        _ => return,
    };
    let _ = record(
        build_dir,
        digest,
        &artifact.tree_root,
        &e.to_string(),
        now_unix(),
    );
}

/// The note of a stage refusal memo that still binds `program`'s `build` at `artifact`'s
/// digests ([`crate::store::StageRefusal::binds`]): why nothing is fetched, and the command
/// that retries it (`update --retry` when `grouped`).
pub(crate) fn digest_refusal_note(
    layout: &Layout,
    program: &str,
    build: u64,
    artifact: &crate::manifest::Artifact,
    grouped: bool,
) -> Option<String> {
    let now = now_unix();
    let memo = crate::store::stage_refusal(&layout.build_dir(program, build))?;
    if !memo.binds(artifact.digest()?, &artifact.tree_root, now) {
        return None;
    }
    let mut how = String::new();
    if memo.signer || memo.held {
        how.push_str("not downloaded again until a new build is published \u{2014} now: ");
    } else {
        let hours = memo
            .retry_after()
            .saturating_sub(now)
            .saturating_add(3599)
            .div_euclid(3600)
            .max(1);
        how.push_str(&memo.attempts.to_string());
        how.push_str(" attempts failed; not downloaded again for about ");
        how.push_str(&u64::try_from(hours).unwrap_or(0).to_string());
        how.push_str("h or until a new build is published \u{2014} now: ");
    }
    if grouped {
        how.push_str("aterm pkg update --retry");
    } else {
        how.push_str("aterm pkg install ");
        how.push_str(program);
    }
    Some(crate::state::held_failed(&memo.why, &how))
}

pub(crate) const fn disk_gate(required: u64, available: Option<u64>) -> Result<(), FlowError> {
    match available {
        Some(avail) if !crate::cost::disk_ok(required, avail, crate::cost::FREE_FLOOR) => {
            Err(FlowError::InsufficientDisk {
                required: required.saturating_add(crate::cost::FREE_FLOOR),
                available: avail,
            })
        }
        _ => Ok(()), // None (query failed): fail open
    }
}

enum InstallNeed {
    /// Every Install member's manifest carries an artifact for the triple.
    Published {
        pins: Vec<PinnedArtifact>,
        bytes: LandingBytes,
    },
    /// `member`'s manifest — bound to its pinned program and `build` — carries no artifact
    /// for the triple: that pin cannot exist on this host.
    Unpublished { member: String, build: u64 },
    /// Nothing proven: a fetch/verify/parse failure.
    Unknown,
}

struct PinnedArtifact {
    program: String,
    /// The pinned build its manifest was fetched for.
    build: u64,
    /// Its artifact for the triple.
    artifact: crate::manifest::Artifact,
}

/// The signed bytes one install unit needs on disk while it lands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct LandingBytes {
    /// The compressed archives (`size`).
    pub asset: u64,
    /// The extracted trees (`[cost].disk_installed`).
    pub tree: u64,
}

impl LandingBytes {
    /// One artifact's bytes.
    pub(crate) const fn of(artifact: &crate::manifest::Artifact) -> Self {
        Self {
            asset: artifact.size,
            tree: artifact.cost.disk_installed,
        }
    }

    /// `self` and `other` landing together, as a tuple's members do.
    #[must_use]
    pub(crate) const fn and(self, other: Self) -> Self {
        Self {
            asset: self.asset.saturating_add(other.asset),
            tree: self.tree.saturating_add(other.tree),
        }
    }

    /// What a disk gate requires free for this unit, before the reserve.
    pub(crate) const fn need(self) -> u64 {
        self.asset.saturating_add(self.tree)
    }
}

/// What a pass that lands `units` one after another needs free, before the reserve, in any order.
pub(crate) fn peak_need(units: impl IntoIterator<Item = LandingBytes>) -> u64 {
    peak_need_at_once(units, 1)
}

/// [`peak_need`] for units landing `lanes` at a time.
pub(crate) fn peak_need_at_once(
    units: impl IntoIterator<Item = LandingBytes>,
    lanes: usize,
) -> u64 {
    let mut trees = 0u64;
    let mut assets = Vec::new();
    for u in units {
        trees = trees.saturating_add(u.tree);
        assets.push(u.asset);
    }
    assets.sort_unstable_by(|a, b| b.cmp(a));
    assets
        .into_iter()
        .take(lanes)
        .fold(trees, u64::saturating_add)
}

/// What the default set's up-front disk check reads of one install unit ([`planned_landing`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlannedLanding {
    /// The pass lands nothing of the unit, so it must not decide whether the rest fits.
    Nothing,
    /// The bytes its landing needs.
    Bytes(LandingBytes),
    /// No answer: a manifest or its row for the triple is unavailable.
    Unknown,
}

/// One install unit of the default set as its up-front disk check reads it.
pub(crate) fn planned_landing(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    ch: &Channel,
    group: &Group,
    installed: &BTreeMap<String, u64>,
    triple: &str,
) -> PlannedLanding {
    let need = if group.group.is_none() {
        let [program] = group.members.as_slice() else {
            return PlannedLanding::Unknown;
        };
        if index
            .program(program)
            .is_some_and(crate::manifest::Program::names_requires)
            || decide(ch, program, None) != ApplyDecision::Install
        {
            return PlannedLanding::Nothing;
        }
        group_install_need(fetcher, index, ch, &[program], triple)
    } else {
        let decided = GroupDecisions::of(layout, ch, group, installed);
        let members = decided.install_members();
        if decided.any_tombstone
            || members.is_empty()
            || decided.retired_requires(index).is_some()
            || decided.pin_hold(layout).is_some()
        {
            return PlannedLanding::Nothing;
        }
        group_install_need(fetcher, index, ch, &members, triple)
    };
    let (pins, bytes) = match need {
        InstallNeed::Published { pins, bytes } => (pins, bytes),
        InstallNeed::Unpublished { .. } => return PlannedLanding::Nothing,
        InstallNeed::Unknown => return PlannedLanding::Unknown,
    };
    let grouped = group.group.is_some();
    if pins
        .iter()
        .any(|p| digest_refusal_note(layout, &p.program, p.build, &p.artifact, grouped).is_some())
    {
        PlannedLanding::Nothing
    } else if pins.iter().any(|p| p.artifact.cost.disk_installed == 0) {
        PlannedLanding::Unknown
    } else {
        PlannedLanding::Bytes(bytes)
    }
}

fn group_install_need(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    ch: &Channel,
    install_members: &[&String],
    triple: &str,
) -> InstallNeed {
    let mut pins = Vec::with_capacity(install_members.len());
    let mut bytes = LandingBytes::default();
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
        bytes = bytes.and(LandingBytes::of(a));
        pins.push(PinnedArtifact {
            program: m.clone(),
            build: pinned,
            artifact: a.clone(),
        });
    }
    InstallNeed::Published { pins, bytes }
}

/// Resolve and verify-select the signed index (cached fallback) and enforce its freshness window.
pub fn resolve_verified_index(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    floor: BuildFloor,
    now_unix: i64,
) -> Result<TrustedIndex, FlowError> {
    let cache = crate::cache::IndexCache::for_layout(layout);
    let src = fetcher.source_id();
    let (candidates, served) = resolve_candidates(fetcher, &cache, &src)?;
    let live = last_resolve().is_some_and(|r| r.reached);
    let index = if served {
        let _ = layout.ensure_dir(&layout.prefix);
        let selected =
            verify_select_fresh_from(layout, anchor, candidates.clone(), floor, now_unix, live);
        if matches!(selected, Err(FlowError::NoIndex))
            && let Some(cached) = cache.load(&src)
        {
            note_resolve(
                false,
                Some(format!("the source served {}", FlowError::NoIndex)),
                true,
            );
            let served_n = candidates.len();
            let both = candidates.iter().chain(&cached).cloned().collect();
            let (verdict, won) = verify_select_at(layout, anchor, both, floor, now_unix, false);
            if let Some(won) = won {
                let mut kept = candidates;
                kept.extend(
                    won.checked_sub(served_n)
                        .and_then(|i| cached.get(i))
                        .cloned(),
                );
                cache.store(&src, &kept);
                return verdict;
            }
            note_resolve(true, None, true);
        }
        cache.store(&src, &candidates);
        selected?
    } else {
        verify_select_fresh_from(layout, anchor, candidates, floor, now_unix, live)?
    };
    if live {
        LIVE_VERIFICATIONS.with(|n| n.set(n.get().saturating_add(1)));
    }
    Ok(index)
}

fn verify_select_fresh(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    floor: BuildFloor,
    now_unix: i64,
) -> Result<TrustedIndex, FlowError> {
    let candidates = fetcher.index_candidates().map_err(FlowError::Unreachable)?;
    verify_select_fresh_from(layout, anchor, candidates, floor, now_unix, true)
}

fn verify_select_fresh_from(
    layout: &Layout,
    anchor: &Anchor,
    candidates: Vec<Candidate>,
    floor: BuildFloor,
    now_unix: i64,
    live: bool,
) -> Result<TrustedIndex, FlowError> {
    verify_select_at(layout, anchor, candidates, floor, now_unix, live).0
}

fn verify_select_at(
    layout: &Layout,
    anchor: &Anchor,
    candidates: Vec<Candidate>,
    floor: BuildFloor,
    now_unix: i64,
    live: bool,
) -> (Result<TrustedIndex, FlowError>, Option<usize>) {
    let pass = select_index(anchor, candidates, floor, now_unix);
    observe_roster_generation(layout, pass.observed_roster_seq);
    let Some(selected) = pass.selected else {
        return (Err(FlowError::NoIndex), None);
    };
    let (index, won) = (selected.index, selected.candidate);
    if !index_is_fresh(&index, now_unix) {
        return (Err(FlowError::Stale), Some(won));
    }
    if layout.prefix.is_dir() {
        use crate::vendor_direct::policy::YankLatch;
        let written = if live {
            YankLatch::update_from_index(layout, &index, now_unix)
        } else {
            YankLatch::carry_from_cached_index(layout, &index)
        };
        if let Err(e) = written {
            eprintln!("atpkg: the vendor yank latch was not written ({e})");
        }
    }
    (Ok(index), Some(won))
}

/// Durably record that this client admitted roster generation `seq`.
pub(crate) fn observe_roster_generation(layout: &Layout, seq: u64) {
    if seq == 0 {
        return;
    }
    let _ = crate::sig::Floor::new(layout.roster_floor()).check_and_record(seq);
}

/// The gate-2 freshness predicate.
pub(crate) fn index_is_fresh(index: &Index, now_unix: i64) -> bool {
    matches!(
        rfc3339_to_unix(&index.valid_until),
        Some(until) if crate::sig::check_freshness(now_unix, until).is_ok()
    )
}

fn resolve_candidates(
    fetcher: &dyn Fetcher,
    cache: &crate::cache::IndexCache,
    src: &str,
) -> Result<(Vec<Candidate>, bool), FlowError> {
    match fetcher.index_candidates() {
        Ok(c) if !c.is_empty() => {
            note_resolve(true, None, true);
            Ok((c, true))
        }
        Ok(_) => match cache.load(src) {
            Some(cached) => {
                note_resolve(false, Some("the source listed no index".into()), true);
                Ok((cached, false))
            }
            None => {
                note_resolve(true, None, true);
                Err(FlowError::NoIndex)
            }
        },
        Err(why) => {
            note_resolve(false, Some(why.clone()), !fetcher.index_link_down());
            cache
                .load(src)
                .map(|c| (c, false))
                .ok_or(FlowError::Unreachable(why))
        }
    }
}

/// What the last index resolve learned about the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveProvenance {
    /// The source served the candidates over the network this resolve (not the cache).
    pub reached: bool,
    /// Why it did not, when it did not — the transport's own sentence.
    pub why: Option<String>,
    /// Whether a host answered at all (the source, or a refusal with a status).
    pub answered: bool,
}

thread_local! {
    static LAST_RESOLVE: std::cell::RefCell<Option<ResolveProvenance>> =
        const { std::cell::RefCell::new(None) };
}

fn note_resolve(reached: bool, why: Option<String>, answered: bool) {
    LAST_RESOLVE.with(|slot| {
        *slot.borrow_mut() = Some(ResolveProvenance {
            reached,
            why,
            answered: reached || answered,
        });
    });
}

thread_local! {
    static LIVE_VERIFICATIONS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// This thread's count of [`resolve_verified_index`] successes on candidates the channel served.
#[must_use]
pub(crate) fn live_verifications() -> u32 {
    LIVE_VERIFICATIONS.with(std::cell::Cell::get)
}

thread_local! {
    static FETCH_FAULTS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Count one member that failed at a fetch (its manifest or artifact did not arrive).
pub(crate) fn note_fetch_fault() {
    FETCH_FAULTS.with(|n| n.set(n.get().saturating_add(1)));
}

/// This thread's [`note_fetch_fault`] count so far.
#[must_use]
pub(crate) fn fetch_faults() -> u32 {
    FETCH_FAULTS.with(std::cell::Cell::get)
}

/// Count, on the pass's thread.
pub(crate) fn note_fetch_faults(n: u32) {
    FETCH_FAULTS.with(|count| count.set(count.get().saturating_add(n)));
}

/// The provenance of this thread's last [`resolve_verified_index`], `None` before the first.
#[must_use]
pub fn last_resolve() -> Option<ResolveProvenance> {
    LAST_RESOLVE.with(|slot| slot.borrow().clone())
}

/// Roll `program` back to the highest retained build strictly below its active build that
/// still passes the floor/yank gate, re-pointing its shims and `current` via
/// [`rollback_member`] (links only; no retained tree is touched).
#[allow(
    clippy::too_many_arguments,
    reason = "rollback needs the fetcher, layout, pinned root key, channel + program \
              selectors, the exclusions the update pass reads, and the floor + clock the \
              freshness/floor gates read — the same irreducible set the install/apply entry \
              points take"
)]
pub fn rollback(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    anchor: &Anchor,
    channel: &str,
    program: &str,
    excluded: &[String],
    floor: BuildFloor,
    now_unix: i64,
) -> Result<RollbackReport, FlowError> {
    let current = *crate::ops::active_builds(layout)
        .get(program)
        .ok_or_else(|| {
            FlowError::Rollback(format!(
                "{program} is not installed \u{2014} nothing to roll back (aterm pkg list shows what is)"
            ))
        })?;
    let index = verify_select_fresh(fetcher, layout, anchor, floor, now_unix)?;
    let coherence_group = index
        .program(program)
        .ok_or_else(|| FlowError::NotReachable(program.to_string(), index_roster(&index)))?
        .coherence_group
        .clone();
    let ch = index
        .channels
        .iter()
        .find(|c| c.name == channel)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    let mut lower: Vec<u64> = crate::ops::list_installed(layout)
        .into_iter()
        .filter(|(p, _)| p == program)
        .map(|(_, b)| b)
        .filter(|&b| b < current)
        .collect();
    lower.sort_unstable();
    lower.dedup();
    let target = lower
        .iter()
        .rev()
        .copied()
        .find(|&b| rollback_lands_on(layout, Some(ch), program, b))
        .ok_or_else(|| {
            FlowError::Rollback(match lower.last() {
                None => {
                    format!("no older {program} build is kept \u{2014} {program} {current} stays")
                }
                Some(&b) if !crate::gate::current_build_ok(ch, program, Some(b)) => {
                    format!("{program} {b} is revoked \u{2014} {program} {current} stays")
                }
                Some(&b) => format!(
                    "{program} {b} has nothing in bin/ to run \u{2014} {program} {current} stays"
                ),
            })
        })?;
    let staged = Staged {
        build: current,
        build_dir: layout.build_dir(program, current),
        exposes: crate::ops::active_tools(layout, program, current),
        prior_build: Some(target),
        was_live: true,
        reloc: None,
        tree_root: String::new(),
        aliases: Aliases::for_program(program, index.program(program)),
    };
    rollback_member(layout, program, &staged);
    let active = crate::ops::active_builds(layout);
    let members = pin_gate_members(layout, &index, ch, program, &active, excluded);
    let revoked_member = members.iter().find_map(|m| {
        let build = active.get(m).copied()?;
        (!crate::gate::current_build_ok(ch, m, Some(build))).then(|| (m.clone(), build))
    });
    let pins = crate::pin::pinned_set(layout);
    let held_by = if revoked_member.is_some() {
        Vec::new()
    } else {
        members.into_iter().filter(|m| pins.contains(m)).collect()
    };
    Ok(RollbackReport {
        program: program.to_string(),
        from_build: current,
        to_build: target,
        index_build: index.index_build,
        roster_seq: index.roster_seq(),
        coherence_group,
        held_by,
        revoked_member,
    })
}

/// The transactional `update <grouped-member>` path.
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
    refuse_vendor_program(program, triple)?;
    let index = verify_select_fresh(fetcher, layout, anchor, floor, now_unix)?;
    let ch = index
        .channel_for(channel, triple)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    refuse_unrouted(layout, &index, &ch, program, triple)?;
    let groups = crate::apply::plan_groups_here(layout, &index, &ch, triple);
    let group = groups
        .into_iter()
        .find(|g| g.members.iter().any(|m| m == program))
        .ok_or_else(|| FlowError::NotPinned(program.to_string()))?;
    Ok(apply_groups(
        fetcher,
        layout,
        &index,
        &ch,
        triple,
        std::slice::from_ref(&group),
        installed,
        excluded,
        &crate::quiet::FlipGate::NOW,
    ))
}

/// Read-only routing decision for the `update` verb.
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
    refuse_vendor_program(program, triple)?;
    let index = verify_select_fresh(fetcher, layout, anchor, floor, now_unix)?;
    let ch = index
        .channel_for(channel, triple)
        .ok_or_else(|| FlowError::NoChannel(channel.to_string()))?;
    refuse_unrouted(layout, &index, &ch, program, triple)?;
    let p = index
        .program(program)
        .ok_or_else(|| FlowError::NotReachable(program.to_string(), index_roster(&index)))?;
    Ok(UpdatePlan {
        group: p.coherence_group.clone(),
        decision: decide(&ch, program, installed_build),
        current_build_ok: crate::gate::current_build_ok(&ch, program, installed_build),
        channel: ch,
    })
}

fn refuse_vendor_program(program: &str, triple: &str) -> Result<(), FlowError> {
    match crate::vendor_direct::spec(program) {
        Some(spec) if !crate::alab::agent(program).is_some_and(|a| a.serves(triple)) => {
            Err(vendor_refusal(program, spec))
        }
        _ => Ok(()),
    }
}

fn refuse_unrouted(
    layout: &Layout,
    index: &Index,
    ch: &Channel,
    program: &str,
    triple: &str,
) -> Result<(), FlowError> {
    match crate::vendor_direct::spec(program) {
        Some(spec) if !crate::alab::source_in(layout, index, ch, program, triple).index_lane() => {
            Err(vendor_refusal(program, spec))
        }
        _ => Ok(()),
    }
}

fn vendor_refusal(program: &str, spec: &crate::vendor_direct::VendorSpec) -> FlowError {
    FlowError::Vendor(format!(
        "{program} is updated from {}, not through the ALab index",
        spec.vendor
    ))
}

/// [`Fetcher::prefetch_manifests`] for the manifests [`verified_pkg`] reads for `members` on `ch`.
pub fn prefetch_pinned<'a>(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    ch: &Channel,
    members: impl IntoIterator<Item = &'a str>,
) {
    let mut pins: Vec<(String, String, u64)> = Vec::new();
    for program in members {
        if let (Some(&build), Some(declared)) = (ch.pin.get(program), index.program(program)) {
            pins.push((declared.repo.clone(), program.to_string(), build));
        }
    }
    if !pins.is_empty() {
        fetcher.prefetch_manifests(&pins);
    }
}

/// The read-only plan `plan_update` yields.
#[derive(Debug, Clone)]
pub struct UpdatePlan {
    pub group: Option<String>,
    pub decision: ApplyDecision,
    pub current_build_ok: bool,
    pub channel: Channel,
}

/// The verified per-build manifest for `program`'s channel pin.
pub(crate) fn verified_pkg(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    ch: &Channel,
    program: &str,
) -> Option<(u64, String, crate::manifest::PkgManifest)> {
    verified_pkg_or_miss(fetcher, index, ch, program).ok()
}

fn verified_pkg_or_miss(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    ch: &Channel,
    program: &str,
) -> Result<(u64, String, crate::manifest::PkgManifest), bool> {
    let pinned = *ch.pin.get(program).ok_or(true)?;
    let repo = index.program(program).ok_or(true)?.repo.clone();
    let (raw, sig) = fetcher
        .pkg_manifest(&repo, program, pinned)
        .map_err(|_| false)?;
    let verified = index.verify_pkg(raw, &sig).map_err(|_| true)?;
    let pkg = parse_pkg(&verified).map_err(|_| true)?;
    Ok((pinned, repo, pkg))
}

fn signed_pkg_of_build(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    repo: &str,
    program: &str,
    build: u64,
) -> Option<crate::manifest::PkgManifest> {
    let (raw, sig) = fetcher.pkg_manifest(repo, program, build).ok()?;
    let verified = index.verify_pkg(raw, &sig).ok()?;
    let pkg = parse_pkg(&verified).ok()?;
    (pkg.is_for(program) && pkg.build_number == build).then_some(pkg)
}

fn signed_root_for(pkg: &crate::manifest::PkgManifest, triple: &str) -> Option<String> {
    pkg.artifact_for(triple)
        .map(|a| a.tree_root.clone())
        .filter(|root| !root.is_empty())
}

/// The version `program`'s signed manifest states for `build`.
pub(crate) fn legacy_facts_of_build(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    repo: &str,
    program: &str,
    build: u64,
    triple: &str,
) -> Option<(String, Option<String>)> {
    let pkg = signed_pkg_of_build(fetcher, index, repo, program, build)?;
    let root = signed_root_for(&pkg, triple);
    Some((pkg.version, root))
}

/// The signed asset size `program`'s pinned build ships for `triple`.
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

/// Recover the signed `tree_root` for an installed build whose attestation row is missing.
pub fn signed_root_for_installed(
    fetcher: &dyn Fetcher,
    index: &TrustedIndex,
    program: &str,
    build: u64,
    triple: &str,
) -> Option<String> {
    let repo = index.program(program)?.repo.clone();
    let pkg = signed_pkg_of_build(fetcher, index, &repo, program, build)?;
    signed_root_for(&pkg, triple)
}

fn stage_failure_note(program: &str, build: u64, why: &str) -> String {
    format!("atpkg: {program} build {build} could not be staged: {why}")
}

/// Stage one group member.
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
    let fetched = fetch_member(
        fetcher,
        layout,
        index,
        ch,
        program,
        triple,
        resolved_assets,
        why,
    )?;
    stage_fetched(layout, index, program, prior_build, why, fetched)
}

struct Fetched {
    pinned: u64,
    pkg: crate::manifest::PkgManifest,
    artifact: crate::manifest::Artifact,
    dl: PathBuf,
    reloc: Option<String>,
    /// Whether the archive was an earlier attempt's, already matched against its signed digest.
    carried: bool,
}

/// [`stage_member`]'s first half.
#[allow(
    clippy::too_many_arguments,
    reason = "the fetch half of stage_member takes its inputs, minus the prior build only the \
              stage half reads"
)]
fn fetch_member(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    ch: &Channel,
    program: &str,
    triple: &str,
    resolved_assets: &mut BTreeMap<String, String>,
    why: &mut String,
) -> Option<Fetched> {
    let (pinned, repo, pkg) = match verified_pkg_or_miss(fetcher, index, ch, program) {
        Ok(found) => found,
        Err(fetched) => {
            if !fetched {
                note_fetch_fault();
            }
            *why = format!("the signed manifest for {program} could not be fetched or verified");
            return None;
        }
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
    resolved_assets.insert(program.to_string(), artifact.asset.clone());
    if let Err(e) = crate::vendor::check_row(artifact, &pkg.exposes) {
        *why = format!("the signed row for {program} was refused: {e}");
        return None;
    }
    let strategy = crate::dispatch::strategy_for(&artifact.kind, &artifact.protocol);
    let reloc = match strategy {
        crate::dispatch::ApplyStrategy::Shim | crate::dispatch::ApplyStrategy::VendorApp => None,
        crate::dispatch::ApplyStrategy::SysrootBundle => Some(artifact.reloc.clone()),
        crate::dispatch::ApplyStrategy::AppBundle | crate::dispatch::ApplyStrategy::Unknown => {
            let note = format!(
                "this aterm cannot install this kind of package ({}/{})",
                artifact.kind, artifact.protocol
            );
            eprintln!("{}", stage_failure_note(program, pinned, &note));
            *why = note;
            return None;
        }
    };
    if let Some(note) = digest_refusal_note(layout, program, pinned, artifact, true) {
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
    if let Err(e) = make_staging_dir(&dl) {
        let parent = dl.parent().unwrap_or(&dl);
        *why = format!("cannot create {}: {e}", parent.display());
        return None;
    }
    crate::progress::note_build(program, pinned);
    let ahead = ahead::claim(program, &dl);
    let carried = ahead.is_none() && carried_archive(&dl, artifact);
    if ahead.is_none() {
        if !carried {
            let _ = std::fs::remove_file(&dl);
        }
        sweep_foreign_partials(&dl);
    }
    if carried {
        println!(
            "atpkg: {program}: reusing verified archive {} from the last attempt",
            artifact.asset
        );
    } else {
        let inline = ahead.is_none();
        let fetched = ahead.unwrap_or_else(|| {
            let _download_watch = crate::progress::watch_download(program, &dl, artifact.size);
            fetch_artifact(fetcher, program, &repo, artifact, &dl)
        });
        if inline && fetched.is_ok() {
            ahead::fetched_inline(program, &dl);
        }
        if let Err(e) = fetched {
            eprintln!("{}", stage_failure_note(program, pinned, &e));
            let _ = std::fs::remove_file(&dl);
            note_fetch_fault();
            *why = format!("the download of {} failed: {e}", artifact.asset);
            return None;
        }
    }
    let artifact = artifact.clone();
    Some(Fetched {
        pinned,
        pkg,
        artifact,
        dl,
        reloc,
        carried,
    })
}

fn stage_fetched(
    layout: &Layout,
    index: &TrustedIndex,
    program: &str,
    prior_build: Option<u64>,
    why: &mut String,
    fetched: Fetched,
) -> Option<Staged> {
    let Fetched {
        pinned,
        pkg,
        artifact,
        dl,
        reloc,
        carried: _,
    } = fetched;
    let artifact = &artifact;
    crate::progress::note_phase(program, crate::progress::Phase::Verify);
    let build_dir = layout.build_dir(program, pinned);
    let was_live =
        std::fs::read_link(layout.program_current(program)).is_ok_and(|t| t == build_dir);
    if let Err(e) = crate::shim_env::write_sidecar(&build_dir, &pkg.shim_env()) {
        let note = format!("its environment file could not be written beside the build: {e}");
        eprintln!("{}", stage_failure_note(program, pinned, &note));
        let _ = std::fs::remove_file(&dl);
        *why = note;
        return None;
    }
    let extract_scope = crate::progress::extract_scope(program, artifact.cost.disk_installed);
    let alab_gate = alab_pack_gate(program, &pkg.exposes);
    let staged = verify_and_stage(
        artifact,
        &dl,
        &build_dir,
        &stage_hooks(alab_gate.as_deref()),
    );
    drop(extract_scope);
    if let Err(e) = staged {
        record_digest_refusal(&build_dir, artifact, &e);
        reclaim_after_failed_stage(&dl, &e);
        eprintln!("{}", stage_failure_note(program, pinned, &e.to_string()));
        *why = format!("staging {} failed: {e}", artifact.asset);
        return None;
    }
    crate::store::clear_stage_refusal(&build_dir);
    let (exposes, refused) = crate::store::split_exposed(&pkg.exposes);
    let _ = crate::store::write_exposes(&build_dir, &exposes);
    let _ = crate::status::record_refused_shims(layout, program, pinned, &refused);
    Some(Staged {
        build: pinned,
        build_dir,
        exposes,
        prior_build,
        was_live,
        reloc,
        tree_root: artifact.tree_root.clone(),
        aliases: Aliases::for_program(program, index.program(program)),
    })
}

fn flip_member(layout: &Layout, program: &str, s: &Staged, ahead: Option<ResolveAhead>) -> bool {
    flip_member_with(layout, program, s, ahead, activate_build)
}

fn flip_member_with(
    layout: &Layout,
    program: &str,
    s: &Staged,
    ahead: Option<ResolveAhead>,
    activate: impl FnOnce(&Layout, &Path) -> std::io::Result<()>,
) -> bool {
    // A sysroot-bundle member gets its pre-activation wiring before the flip; a
    // failure here aborts the group (the payload is staged but never activated).
    if let Some(reloc) = &s.reloc
        && apply_sysroot_bundle(reloc).is_err()
    {
        return false;
    }
    if activate(layout, &s.build_dir).is_err() {
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
    let env = crate::shim_env::read_sidecar(&s.build_dir);
    if install_tools_env(layout, &s.build_dir, &s.exposes, s.aliases, &env).is_err() {
        rollback_member(layout, program, s);
        return false;
    }
    if s.reloc.is_some()
        && ahead
            .map_or_else(
                || bundle_resolve_check(&s.build_dir, &s.exposes),
                ResolveAhead::verdict,
            )
            .is_err()
    {
        rollback_member(layout, program, s);
        return false;
    }
    true
}

struct ResolveAhead(Option<std::thread::JoinHandle<Result<(), FlowError>>>);

impl ResolveAhead {
    fn start(s: &Staged) -> Option<Self> {
        #[cfg(test)]
        if let Some(check) = RESOLVE_CHECK.with(|c| c.borrow().clone()) {
            return Self::start_with(s, move |build_dir, exposes| check(build_dir, exposes));
        }
        Self::start_with(s, |build_dir, exposes| {
            bundle_resolve_check(build_dir, exposes)
        })
    }

    fn start_with(
        s: &Staged,
        check: impl FnOnce(&Path, &[ToolName]) -> Result<(), FlowError> + Send + 'static,
    ) -> Option<Self> {
        let reloc = s.reloc.as_deref()?;
        apply_sysroot_bundle(reloc).ok()?;
        let (build_dir, exposes) = (s.build_dir.clone(), s.exposes.clone());
        std::thread::Builder::new()
            .name("atpkg-resolve-ahead".into())
            .spawn(move || check(&build_dir, &exposes))
            .ok()
            .map(|handle| Self(Some(handle)))
    }

    fn verdict(mut self) -> Result<(), FlowError> {
        let refused = || FlowError::Activate("resolve check: it answered no verdict".to_string());
        let handle = self.0.take().ok_or_else(refused)?;
        handle.join().unwrap_or_else(|_| Err(refused()))
    }
}

impl Drop for ResolveAhead {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
type InjectedResolveCheck =
    std::sync::Arc<dyn Fn(&Path, &[ToolName]) -> Result<(), FlowError> + Send + Sync>;

#[cfg(test)]
thread_local! {
    static RESOLVE_CHECK: std::cell::RefCell<Option<InjectedResolveCheck>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn with_resolve_check<R>(check: InjectedResolveCheck, f: impl FnOnce() -> R) -> R {
    let prior = RESOLVE_CHECK.with(|c| c.replace(Some(check)));
    let out = f();
    RESOLVE_CHECK.with(|c| *c.borrow_mut() = prior);
    out
}

pub(crate) fn make_staging_dir(dl: &Path) -> std::io::Result<()> {
    match dl.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            crate::platform::make_dirs_born(parent, 0o700)
        }
        _ => Ok(()),
    }
}

pub(crate) fn staged_download_path(
    layout: &Layout,
    program: &str,
    asset: &str,
) -> Result<PathBuf, FlowError> {
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

/// Whether [`rollback`] may land `program` on its retained `build`.
pub(crate) fn rollback_lands_on(
    layout: &Layout,
    ch: Option<&Channel>,
    program: &str,
    build: u64,
) -> bool {
    ch.is_none_or(|ch| crate::gate::current_build_ok(ch, program, Some(build)))
        && build_can_be_rolled_onto(layout, program, build)
}

fn build_can_be_rolled_onto(layout: &Layout, program: &str, build: u64) -> bool {
    let bin = layout.build_dir(program, build).join("bin");
    match std::fs::read_dir(&bin) {
        Ok(entries) => entries.flatten().next().is_some(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

fn rollback_member(layout: &Layout, program: &str, s: &Staged) {
    match s.prior_build {
        Some(prior) if prior != s.build => {
            let prior_dir = layout.build_dir(program, prior);
            let mut restore: std::collections::BTreeSet<crate::store::ToolName> =
                s.exposes.iter().cloned().collect();
            let mut listing_only: std::collections::BTreeSet<crate::store::ToolName> =
                std::collections::BTreeSet::new();
            if let Ok(entries) = std::fs::read_dir(prior_dir.join("bin")) {
                for e in entries.flatten() {
                    // Files only: a stray subdirectory is not a tool.
                    if !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                        continue;
                    }
                    if let Some(name) = e.file_name().to_str() {
                        let logical = name
                            .strip_suffix(crate::platform::EXE_SUFFIX)
                            .unwrap_or(name);
                        if let Some(tool) = crate::store::ToolName::new(logical)
                            && !restore.contains(&tool)
                            && rollback_may_claim(layout, program, &tool)
                        {
                            listing_only.insert(tool.clone());
                            restore.insert(tool);
                        }
                    }
                }
            }
            if let Err(e) = activate_build(layout, &prior_dir) {
                eprintln!(
                    "atpkg: {program}: the rollback to build {prior} could not re-point \
                     `current` ({e}) — its shims are re-laid all the same"
                );
            }
            let env = crate::shim_env::read_sidecar(&prior_dir);
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
                let alias = (s.aliases == Aliases::Alab)
                    .then(|| tool.alias())
                    .flatten()
                    .filter(|alias| {
                        !listing_only.contains(tool)
                            || shim_free_or_owned_by(layout, program, &layout.shim(alias))
                    });
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
                    let _ = crate::lay::remove_in_process(&layout.shim(tool));
                    if let Some(alias) = &alias {
                        let _ = crate::lay::remove_in_process(&layout.shim(alias));
                    }
                }
            }
        }
        Some(_) => { /* prior == new build: nothing meaningful to undo */ }
        None => {
            for tool in &s.exposes {
                let _ = crate::lay::remove_in_process(&layout.shim(tool));
                if s.aliases == Aliases::Alab
                    && let Some(alias) = tool.alias()
                {
                    let _ = crate::lay::remove_in_process(&layout.shim(&alias));
                }
            }
            crate::platform::remove_link(&layout.program_current(program));
        }
    }
    crate::activate::reconcile_agents(layout);
}

fn rollback_may_claim(layout: &Layout, program: &str, tool: &ToolName) -> bool {
    if tool.as_str() != program && another_program_is_installed_as(layout, tool.as_str()) {
        return false;
    }
    shim_free_or_owned_by(layout, program, &layout.shim(tool))
}

fn another_program_is_installed_as(layout: &Layout, name: &str) -> bool {
    if std::fs::symlink_metadata(layout.program_current(name)).is_ok() {
        return true;
    }
    let Ok(builds) = std::fs::read_dir(layout.prefix.join("store").join(name)) else {
        return false;
    };
    builds.flatten().any(|b| {
        b.file_name()
            .to_str()
            .and_then(crate::store::parse_build_name)
            .is_some_and(|_| crate::store::build_is_complete(&b.path()))
    })
}

fn shim_free_or_owned_by(layout: &Layout, program: &str, shim: &Path) -> bool {
    match std::fs::symlink_metadata(shim) {
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        Ok(_) => crate::platform::resolve_shim(shim)
            .and_then(|t| crate::ops::store_build_of(&layout.prefix, &t))
            .is_some_and(|(owner, _)| owner == program),
    }
}

fn apply_sysroot_bundle(reloc: &str) -> Result<(), FlowError> {
    match reloc {
        "self-contained" => Ok(()),
        other => Err(FlowError::UnsupportedKind(format!("reloc={other}"))),
    }
}

/// Run the fail-loud [`crate::sysroot::resolve_check`] over every exposed binary a bundle ships.
pub(crate) fn bundle_resolve_check(
    build_dir: &Path,
    exposes: &[ToolName],
) -> Result<(), FlowError> {
    bundle_resolve_check_with(build_dir, exposes, &crate::sysroot::resolve_check)
}

fn bundle_resolve_check_with(
    build_dir: &Path,
    exposes: &[ToolName],
    check: &(dyn Fn(&Path) -> Result<(), String> + Sync),
) -> Result<(), FlowError> {
    let bins: Vec<PathBuf> = exposes
        .iter()
        .map(|tool| build_dir.join("bin").join(tool.exe_file()))
        .filter(|bin| {
            std::fs::canonicalize(bin).is_ok_and(|object| crate::macho::is_native_object(&object))
        })
        .collect();
    crate::net::fan_out(&bins, &|bin: &PathBuf| check(bin))
        .into_iter()
        .collect::<Result<(), String>>()
        .map_err(FlowError::Activate)
}

/// Current Unix epoch second, the `now_unix` every freshness gate takes as a parameter.
pub(crate) use aterm_update_core::roster::now_unix;

/// Parse an RFC3339 UTC timestamp `YYYY-MM-DDTHH:MM:SSZ` to a Unix epoch second.
pub(crate) use aterm_types::rfc3339::parse_utc as rfc3339_to_unix;

#[cfg(test)]
#[path = "flow_tests.rs"]
mod tests;
