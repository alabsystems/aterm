// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pre-claim gates, each failing closed with its remedy before anything is committed or pushed.

use aterm_uds::spawnfd::{Command, command};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::changelog;
use crate::channel;
use crate::ledger::{BYTE_EXACT, Error, GitRunner, Result, git_ok, rev_parse};
use crate::release::Refusal;

/// Free-disk floor for the release's build and its cut.
pub const MIN_FREE_DISK_GIB: u64 = 10;

/// Inputs of qualify's immutable fixture and policy judgments. The advertised
/// tag set is external input too: a new predecessor must never inherit an old
/// judgment merely because the source and compiler stayed fixed.
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommitGateRecord {
    source: String,
    version: String,
    inputs: String,
    tags: String,
}

impl CommitGateRecord {
    pub(crate) fn new(source: &str, version: &str, inputs: String, tags: String) -> Self {
        Self {
            source: source.into(),
            version: version.into(),
            inputs,
            tags,
        }
    }

    fn path(git: &dyn GitRunner) -> Result<PathBuf> {
        Ok(crate::release::release_dir(git)
            .map_err(Error::new)?
            .join("commit-gates.toml"))
    }

    pub(crate) fn save(&self, git: &dyn GitRunner) -> Result<()> {
        let text = aterm_toml::to_string(self).map_err(|e| Error::new(e.to_string()))?;
        crate::durable::write_atomic_durable(&Self::path(git)?, text.as_bytes(), 0o600)
            .map_err(|e| Error::new(e.to_string()))
    }

    pub(crate) fn adopt(&self, git: &dyn GitRunner) -> Result<()> {
        let record = read_bounded_string(&Self::path(git)?, 1 << 20)
            .map_err(|e| Error::new(format!("read qualify's commit gates: {e}")))?;
        let recorded: Self = aterm_toml::from_str(&record)
            .map_err(|e| Error::new(format!("parse qualify's commit gates: {e}")))?;
        self.agrees(&recorded)
    }

    fn agrees(&self, recorded: &Self) -> Result<()> {
        if self != recorded {
            return Err(Error::new(
                "qualify's fixture/policy record has another source, version, inputs seal or advertised tag set; nothing was claimed",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "gates_commit_gate_record_tests.rs"]
mod commit_gate_record_tests;

/// The Trust toolchain's stage2 tool dir (`targo`, `trustc`, `trustdoc`, …), found by atpkg's one resolution order.
pub fn trust_stage2_bin() -> Result<PathBuf> {
    let pinned = pin_first_success(&PINNED, resolve_trust_stage2_bin)?;
    if LEASE_ARMED.load(std::sync::atomic::Ordering::Relaxed) {
        lease_toolchain_checked(&pinned).map_err(Error::new)?;
    }
    Ok(pinned)
}

static PINNED: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Pin `dir` as this process's Trust toolchain before anything resolves one.
///
/// # Errors
/// The directory cannot be resolved, or this process already pinned another toolchain.
pub fn pin_trust_stage2_bin(dir: &Path) -> Result<()> {
    let dir = canonical_toolchain(dir)?;
    pin_into(&PINNED, &dir)
}

fn canonical_toolchain(dir: &Path) -> Result<PathBuf> {
    dir.canonicalize()
        .map_err(|e| Error::new(format!("resolve toolchain {}: {e}", dir.display())))
}

fn pin_into(cell: &std::sync::OnceLock<PathBuf>, dir: &Path) -> Result<()> {
    let pinned = cell.get_or_init(|| dir.to_path_buf());
    if pinned == dir {
        Ok(())
    } else {
        Err(Error::new(format!(
            "this process already pinned the toolchain {}, not {}",
            pinned.display(),
            dir.display()
        )))
    }
}

static LEASE_ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
type ToolchainLeases = std::collections::BTreeMap<PathBuf, atpkg::lease::Lease>;
static TOOLCHAIN_LEASES: std::sync::Mutex<ToolchainLeases> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// One exact close-on-exec duplicate of the selected toolchain lease, with its bounded
/// holder-file locator. The receiver derives the store and subject independently.
#[cfg(unix)]
#[derive(Debug)]
pub struct ToolchainLeaseOffer {
    /// A close-on-exec duplicate of the original held lease description.
    pub descriptor: std::os::fd::OwnedFd,
    /// Its generated direct-child filename, never a supplied path.
    pub leaf: String,
}

/// Arm [`lease_toolchain_checked`] for every subsequent toolchain resolution.
pub fn arm_toolchain_lease() {
    LEASE_ARMED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Hold the selected compiler before inspecting it; refuse a store lease failure.
/// # Errors
/// The directory cannot be resolved, or the store entry's lease cannot be acquired.
pub fn lease_toolchain_checked(dir: &Path) -> std::result::Result<(), String> {
    let dir = dir
        .canonicalize()
        .map_err(|e| format!("resolve toolchain {} before leasing: {e}", dir.display()))?;
    let mut leases = TOOLCHAIN_LEASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if leases.contains_key(&dir) {
        return Ok(());
    }
    let home = aterm_types::dirs::home_dir();
    let configured = atpkg::config::load().prefix_path(home.as_deref());
    let Some(layout) = atpkg::store::resolve(configured.as_deref()) else {
        return Ok(());
    };
    retain_cut_lease(&mut leases, &layout.prefix, &dir)
}

/// Duplicate the retained lease for `dir` for one controlled in-place exec. An unmanaged
/// toolchain has no atpkg lease and returns `None`.
///
/// # Errors
/// The toolchain cannot be resolved or leased, its retained slot changed, its generated leaf
/// is not representable, or its descriptor cannot be duplicated.
#[cfg(unix)]
pub fn offer_toolchain_lease(
    dir: &Path,
) -> std::result::Result<Option<ToolchainLeaseOffer>, String> {
    let dir = dir.canonicalize().map_err(|e| {
        format!(
            "resolve toolchain {} before lease handover: {e}",
            dir.display()
        )
    })?;
    lease_toolchain_checked(&dir)?;
    let leases = TOOLCHAIN_LEASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    offer_cut_lease(&leases, &dir)
}

#[cfg(unix)]
fn offer_cut_lease(
    leases: &ToolchainLeases,
    dir: &Path,
) -> std::result::Result<Option<ToolchainLeaseOffer>, String> {
    let Some(lease) = leases.get(dir) else {
        return Ok(None);
    };
    let leaf = lease
        .path()
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| {
            format!(
                "toolchain lease {} has no UTF-8 leaf",
                lease.path().display()
            )
        })?
        .to_owned();
    let descriptor = lease
        .try_clone_descriptor()
        .map_err(|e| format!("duplicate toolchain lease {}: {e}", lease.path().display()))?;
    Ok(Some(ToolchainLeaseOffer { descriptor, leaf }))
}

/// Adopt an exact inherited toolchain lease for `dir` into the process-wide retained set.
/// The configured atpkg prefix and canonical toolchain directory are selected here; `leaf`
/// supplies no pathname or subject authority.
///
/// The owned descriptor is consumed on every answer.
///
/// # Safety
/// Call at image startup before threads or callbacks can close, replace or use `fd`'s raw number,
/// under the exclusive continuous custody required by
/// [`atpkg::lease::Lease::adopt_inherited`].
///
/// # Errors
/// The configured store, canonical toolchain, original lease description or exact subject leaf
/// cannot be established, or this image already retained a lease for the directory.
#[cfg(unix)]
pub unsafe fn adopt_toolchain_lease(
    fd: std::os::fd::OwnedFd,
    dir: &Path,
    leaf: &str,
) -> std::result::Result<(), String> {
    let dir = dir
        .canonicalize()
        .map_err(|e| format!("resolve inherited toolchain {}: {e}", dir.display()))?;
    let home = aterm_types::dirs::home_dir();
    let configured = atpkg::config::load().prefix_path(home.as_deref());
    let layout = atpkg::store::resolve(configured.as_deref()).ok_or_else(|| {
        format!(
            "cannot resolve the configured atpkg store for inherited toolchain {}",
            dir.display()
        )
    })?;
    let mut leases = TOOLCHAIN_LEASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: the caller supplies the exclusive startup custody. The prefix and canonical
    // directory were selected independently in this image.
    unsafe { adopt_cut_lease(&mut leases, &layout.prefix, &dir, fd, leaf) }
}

#[cfg(unix)]
unsafe fn adopt_cut_lease(
    leases: &mut ToolchainLeases,
    prefix: &Path,
    dir: &Path,
    fd: std::os::fd::OwnedFd,
    leaf: &str,
) -> std::result::Result<(), String> {
    let std::collections::btree_map::Entry::Vacant(entry) = leases.entry(dir.to_path_buf()) else {
        return Err(format!(
            "this image already retained the toolchain lease for {}",
            dir.display()
        ));
    };
    // SAFETY: propagated from this startup-only helper's caller; prefix and dir are receiver
    // selections and Lease validates the leaf against the subject it derives from them.
    let lease = unsafe { atpkg::lease::Lease::adopt_inherited(fd, prefix, dir, leaf) }
        .map_err(|e| format!("adopt toolchain lease for {}: {e}", dir.display()))?;
    entry.insert(lease);
    Ok(())
}

fn retain_cut_lease(
    leases: &mut std::collections::BTreeMap<PathBuf, atpkg::lease::Lease>,
    prefix: &Path,
    dir: &Path,
) -> std::result::Result<(), String> {
    if let std::collections::btree_map::Entry::Vacant(entry) = leases.entry(dir.to_path_buf())
        && let Some(lease) = take_cut_lease_checked(prefix, dir)?
    {
        entry.insert(lease);
    }
    Ok(())
}

#[cfg(test)]
fn take_cut_lease(prefix: &Path, dir: &Path) -> Option<atpkg::lease::Lease> {
    take_cut_lease_checked(prefix, dir).unwrap()
}

fn take_cut_lease_checked(
    prefix: &Path,
    dir: &Path,
) -> std::result::Result<Option<atpkg::lease::Lease>, String> {
    let who = format!(
        "aterm-release (pid {}) \u{2014} a release cut",
        std::process::id()
    );
    match atpkg::lease::take_for_dir(prefix, dir, &who) {
        Ok(Some(lease)) => {
            eprintln!(
                "release: {} held for this cut; an unattended trust update waits for it",
                lease.subject().describe()
            );
            Ok(Some(lease))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(format!(
            "toolchain lease refused for {}: {e}",
            dir.display()
        )),
    }
}

fn pin_first_success(
    cell: &std::sync::OnceLock<PathBuf>,
    resolve: impl FnOnce() -> Result<PathBuf>,
) -> Result<PathBuf> {
    if let Some(pinned) = cell.get() {
        return Ok(pinned.clone());
    }
    let resolved = resolve()?;
    Ok(cell.get_or_init(|| resolved).clone())
}

fn resolve_trust_stage2_bin() -> Result<PathBuf> {
    let explicit = env::var_os("TRUST_STAGE2_BIN").map(PathBuf::from);
    let home = aterm_types::dirs::home_dir().unwrap_or_default();
    let path = env::var_os("PATH").unwrap_or_default();
    let mut before_probe = |dir: &Path| {
        if LEASE_ARMED.load(std::sync::atomic::Ordering::Relaxed) {
            lease_toolchain_checked(dir)
        } else {
            Ok(())
        }
    };
    // The clone's working-tree lock names the build here, independently of the store's `current`.
    // Binding the chosen candidate's own lock remains A14 release integration work: move prover
    // resolution after candidate selection and use `trust_lock::locked_prover` over `lock_at(R)`.
    let lock = env::current_dir().ok().and_then(|cwd| {
        let xdg = env::var_os("XDG_CONFIG_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from);
        let prefix = atpkg::toolchain::atpkg_prefix(&home, xdg.as_deref());
        atpkg::trust_lock::found_for(&cwd, Some(&prefix))
    });
    let found = atpkg::toolchain::Toolchain::discover_guarded(
        explicit.as_deref(),
        &home,
        &path,
        Some(atpkg::seam::DEFAULT_SEAM),
        lock.as_ref(),
        &mut before_probe,
    )
    .map_err(Error::new)?;
    if found.have_targo() && found.stage2_dir.join("trustc").is_file() {
        return canonical_toolchain(&found.stage2_dir);
    }
    if let Some(why) = &found.lock_refusal {
        return Err(Error::new(why.clone()));
    }
    let looked = found
        .refused
        .as_ref()
        .map(|dir| format!("{} (not the pinned `trust` toolchain)", dir.display()))
        .unwrap_or_else(|| found.stage2_dir.display().to_string());
    Err(Error::new(if explicit.is_some() {
        format!(
            "no Trust toolchain at TRUST_STAGE2_BIN={looked} (an explicit override is never \
             fallen back from)"
        )
    } else {
        format!("no Trust toolchain found\nlooked in: {looked}")
    }))
}

/// The way past a missing x86_64 slice, for a caller to append verbatim under its own `fix:` marker: `verify provision`'s rustup row puts the release chain's stable in place with that slice's std (`aterm_verify::provision::stock::RELEASE_TOOLCHAIN`), where a hand `rustup +stable target add` fails on a home whose auto-install it turned off and that holds no stable.
pub const X86_SLICE_REMEDIES: &str = "targo --unverified verify provision rustup";

/// How to build Trust from source, for every remedy that offers it.
macro_rules! trust_from_source {
    () => {
        concat!(
            "in a Trust checkout, ",
            atpkg::toolchain::trust_recreate_bootstrap_cmd!()
        )
    };
}

/// The from-source build, for a remedy to name after "from source" (`trust_from_source!`).
pub const TRUST_FROM_SOURCE: &str = trust_from_source!();

/// The three ways to get a Trust toolchain, for a caller to append verbatim.
pub const TRUST_TOOLCHAIN_REMEDIES: &str = concat!(
    "aterm pkg install trust   (then `aterm pkg doctor` to confirm the store)\n\
     or, from source: ",
    trust_from_source!(),
    "\n\
     or point TRUST_STAGE2_BIN at an existing stage2 bin dir"
);

/// The `targo` build driver from the stage2 tool dir.
pub fn resolve_targo() -> Result<PathBuf> {
    let targo = trust_stage2_bin()?.join("targo");
    if targo.is_file() {
        Ok(targo)
    } else {
        Err(Error::new(format!(
            "targo missing at {} — the stage2 toolchain is incomplete; reinstall it \
             (`aterm pkg install trust`, then `aterm pkg doctor`), or point TRUST_STAGE2_BIN \
             at a stage2 bin dir that carries targo",
            targo.display()
        )))
    }
}

/// The gate knobs that come off the `cut` command line.
pub struct GateOpts {
    /// Release version being cut, such as "0.2.0" — drives the tag gates.
    pub version: String,
    /// This cut publishes nowhere — `--dry-run` (no uploads at all).
    pub offline: bool,
    /// Only a `--dry-run` may execute from a cutter older than the tree.
    pub allow_stale_cutter: bool,
    /// A real cut's published commit, already placed in the cut tree ([`place_published`]).
    pub published: Option<PublishedCheckout>,
}

/// What the gates learned, for the cut transcript's `gates` lines.
pub struct GateReport {
    /// Short (8-hex) HEAD sha for the banner.
    pub head_short: String,
    /// The login the release token speaks as (`GET /user`); `None` when none was asked.
    pub github_login: Option<String>,
    /// Free disk in GiB at gate time.
    pub free_disk_gib: u64,
    /// `Some(version)` when the public update channel's source tree was read and carries exactly this version.
    pub channel_version: Option<String>,
    /// How many running processes the staging-liveness gate read.
    pub processes_checked: usize,
    /// The published commit this cut releases — `None` for a dry run.
    pub published: Option<PublishedCheckout>,
}

/// Run every gate that depends on the cut's moment, in the transcript's order.
pub fn run_all(
    git: &dyn GitRunner,
    origin: Option<&dyn crate::ledger::Origin>,
    tree: &Path,
    state: &Path,
    opts: &GateOpts,
) -> Result<GateReport> {
    use crate::cut_timing::measure;
    measure("host", host_gate)?;
    measure("submodule gitlinks", || submodules_at_gitlinks(git))?;
    measure("clean tree", || clean_tree(git))?;
    let head = measure("HEAD", || rev_parse(git, "HEAD"))?;
    if let Some(published) = &opts.published
        && head != published.source.commit
    {
        return Err(Error::new(format!(
            "HEAD ({head}) is not the published commit ({}) the cut placed — something \
             moved the cut tree {}; nothing was claimed, cut again",
            published.source.commit,
            published.tree.display()
        )));
    }
    measure("own cutter", || {
        own_cutter_gate(git, &head, opts.allow_stale_cutter)
    })?;
    measure("tag free", || {
        tag_gate(git, origin, &opts.version, !opts.offline)
    })?;
    let github_login = measure("GitHub identity", || github_identity_gate(opts.offline))?;
    let free_disk_gib = measure("disk", || disk_gate(tree))?;
    let processes_checked = measure("staging process scan", || {
        staged_bundle_liveness_gate(&state.join("dist"), &crate::bundle::running_processes)
    })?;
    let channel_version = measure("public channel", || {
        channel_version_gate(tree, &opts.version, opts.offline)
    })?;
    Ok(GateReport {
        head_short: head.chars().take(8).collect(),
        github_login,
        free_disk_gib,
        channel_version,
        processes_checked,
        published: opts.published.clone(),
    })
}

/// Refuse a release whose committed handoff policy (`publish/handoff-policy.toml`) the producers it is meant for could not follow, judged on the commit before anything is staged.
///
/// # Errors
/// The commit holds no policy, or one producers could not follow; its ledger cannot be read or
/// parsed, origin's tags cannot be listed, or none of the ledger's earlier versions shipped (no
/// predecessor can be named); or the policy drops what its predecessor sealed without retiring it.
pub fn handoff_policy_gate_at(
    git: &dyn GitRunner,
    origin: &dyn crate::ledger::Origin,
    commit: &str,
    version: &str,
) -> Result<CutHandoffPolicy> {
    use aterm_update_core::handoff_policy::SOURCE_PATH;
    let refused = |why: String| Error::new(format!("handoff-policy gate: {why}"));
    let tree = GateTree::Commit { git, commit };
    let text = tree
        .text(SOURCE_PATH)
        .map_err(|e| refused(crate::bundle::handoff_policy_unreadable(&e.to_string())))?;
    let policy = crate::bundle::handoff_policy_of_text(
        &text,
        &format!("{SOURCE_PATH} at {}", short(commit)),
    )
    .map_err(refused)?;
    let predecessor = predecessor_in(origin, &tree, version).map_err(|e| refused(e.0))?;
    sealed_policy_kept(git, origin, &tree, predecessor.as_deref(), policy, &text)
}

/// What [`handoff_policy_gate_at`] judged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CutHandoffPolicy {
    pub policy: aterm_update_core::handoff_policy::PolicyFile,
    /// The predecessor release and the file's `[[retired]]` entry, for each rescue that release sealed and this cut drops.
    pub retires: Vec<(String, aterm_update_core::handoff_policy::Retired)>,
}

impl std::fmt::Display for CutHandoffPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.policy)?;
        for (release, retired) in &self.retires {
            write!(
                formatter,
                "; RETIRES the rescue v{release} sealed for builds [{}, {}] ({}): {}",
                retired.producers.min_build,
                retired.producers.max_build,
                retired.date,
                retired.reason
            )?;
        }
        Ok(())
    }
}

fn sealed_policy_kept(
    git: &dyn GitRunner,
    origin: &dyn crate::ledger::Origin,
    tree: &GateTree<'_>,
    predecessor: Option<&str>,
    policy: aterm_update_core::handoff_policy::PolicyFile,
    text: &str,
) -> Result<CutHandoffPolicy> {
    use aterm_update_core::handoff_policy::{
        FIRST_POLICY_FOLLOWING_BUILD, PolicyFile, ProducerRange, Retired, SOURCE_PATH,
    };
    let retired = Retired::parse_for_cut(text)
        .map_err(|why| Error::new(format!("handoff-policy gate: {SOURCE_PATH}: {why}")))?;
    let handing_over = match predecessor {
        Some(release) => builds_handing_over(&tree.text(crate::ledger::LEDGER_FILE)?, release)?,
        None => Vec::new(),
    };
    policy.names_producers(&handing_over).map_err(|why| {
        let fix = match (predecessor, handing_over.iter().max()) {
            (Some(release), Some(newest)) if *newest >= FIRST_POLICY_FOLLOWING_BUILD => format!(
                "name builds {} claimed for the releases the rescue is for, from \
                 {FIRST_POLICY_FOLLOWING_BUILD} (a producer before it follows no policy, so \
                 its bug is the new release's to absorb on its own side) to {newest} \
                 (v{release}'s)",
                crate::ledger::LEDGER_FILE
            ),
            _ => "set no knob: no build that can hand over to this release follows a policy"
                .to_string(),
        };
        Error::new(format!(
            "handoff-policy gate: this cut's {SOURCE_PATH} ({policy}) does not name the builds it \
             rescues: {why}.\nfix:  {fix}"
        ))
    })?;
    let Some(release) = predecessor else {
        return Ok(CutHandoffPolicy {
            policy,
            retires: Vec::new(),
        });
    };
    let shipped = shipped_commit(origin, release)?;
    let listed = git_ok(
        git,
        &["ls-tree", "--name-only", &shipped, "--", SOURCE_PATH],
    )?;
    let unreadable = |why: String| {
        Error::new(format!(
            "handoff-policy gate: this cutter cannot read the policy v{release} sealed ({why}), \
             so it cannot tell what this cut must keep"
        ))
    };
    let sealed = if listed.stdout_utf8().trim().is_empty() {
        Vec::new()
    } else {
        let sealed_text = git_ok(
            git,
            &["cat-file", "blob", &format!("{shipped}:{SOURCE_PATH}")],
        )?
        .stdout_utf8();
        PolicyFile::parse(&sealed_text)
            .and_then(|file| file.rescues())
            .map_err(unreadable)?
    };
    let range_of = |range: ProducerRange| format!("[{}, {}]", range.min_build, range.max_build);
    let mut retires = Vec::new();
    for rescue in &sealed {
        let named = rescue.policy.applies_to_producers;
        let entry = retired
            .iter()
            .find(|retired| Some(retired.producers) == named);
        match (policy.keeps(&rescue.policy), entry) {
            (Ok(()), None) => {}
            (Ok(()), Some(entry)) => {
                return Err(Error::new(format!(
                    "handoff-policy gate: {SOURCE_PATH} lists builds {} under `[[retired]]`, but \
                     it keeps the rescue v{release} sealed for them ({}).\n\
                     fix:  delete that `[[retired]]` table, or drop the rescue",
                    range_of(entry.producers),
                    rescue.policy
                )));
            }
            (Err(_), Some(entry)) => retires.push((release.to_string(), entry.clone())),
            (Err(why), None) => {
                let builds = named.map_or_else(
                    || "every build".to_string(),
                    |range| format!("builds {}", range_of(range)),
                );
                let reason = if rescue.reason.is_empty() {
                    String::new()
                } else {
                    format!(", \"{}\"", rescue.reason)
                };
                let near = named
                    .and_then(|range| {
                        retired.iter().find(|retired| {
                            retired.producers.min_build <= range.max_build
                                && range.min_build <= retired.producers.max_build
                        })
                    })
                    .map_or(String::new(), |retired| {
                        format!(
                            " (a `[[retired]]` table names builds {}, not that range)",
                            range_of(retired.producers)
                        )
                    });
                let retire = named.map_or(String::new(), |range| {
                    format!(
                        ", or — once no build in that range can still be installed — retire \
                         it: a `[[retired]]` table with `producers = {}`, its `reason` and \
                         today's `date`",
                        range_of(range)
                    )
                });
                return Err(Error::new(format!(
                    "handoff-policy gate: this cut's {SOURCE_PATH} drops what v{release} sealed \
                     for {builds} ({}{reason}): {why}{near}. A producer reads only the release \
                     it hands over to, so a build that skipped v{release} is protected only \
                     while every later release keeps what v{release} sealed.\n\
                     fix:  keep that rescue as a `[[ranges]]` table (a wider range or a more \
                     conservative knob keeps it too, and the top-level keys stay the hull of \
                     every table){retire}",
                    rescue.policy
                )));
            }
        }
    }
    Ok(CutHandoffPolicy { policy, retires })
}

fn builds_handing_over(ledger_text: &str, release: &str) -> Result<Vec<u64>> {
    let records = crate::ledger::parse(ledger_text)?;
    let Some(newest) = records
        .iter()
        .filter(|record| record.version == release)
        .map(|record| record.build)
        .max()
    else {
        return Err(Error::new(format!(
            "handoff-policy gate: {} records no build for v{release}, the release this cut \
             succeeds",
            crate::ledger::LEDGER_FILE
        )));
    };
    Ok(records
        .iter()
        .map(|record| record.build)
        .filter(|&build| build <= newest)
        .collect())
}

fn shipped_commit(origin: &dyn crate::ledger::Origin, release: &str) -> Result<String> {
    let Some(commit) = origin
        .tag_commit(&format!("v{release}"))
        .map_err(Error::new)?
    else {
        return Err(Error::new(format!(
            "handoff-policy gate: origin no longer carries v{release}, the release this cut \
             succeeds; nothing was claimed — run it again"
        )));
    };
    Ok(commit)
}

/// No live bundle under the cut's `rm -rf`.
pub fn staged_bundle_liveness_gate(
    dist: &Path,
    lister: &dyn Fn() -> Option<Vec<crate::bundle::RunningProcess>>,
) -> Result<usize> {
    let Some(processes) = lister() else {
        return Err(Error::new(format!(
            "cannot read the process table, so nothing proves no aterm runs out of a cut \
             staging bundle under {}. Nothing was claimed; cut again.",
            dist.display()
        )));
    };
    let live = crate::bundle::live_staging_dirs(dist, &processes);
    if live.is_empty() {
        return Ok(processes.len());
    }
    let named: Vec<String> = live
        .iter()
        .map(|(dir, pids)| {
            format!(
                "pid {} from {}",
                pids.join(", "),
                dir.join("aterm.app").display()
            )
        })
        .collect();
    Err(Error::new(format!(
        "a process is running out of a cut staging bundle: {}. Deleting it under a live \
         process resets the TCC grants of every aterm on this Mac. Nothing was claimed.\n\
         fix:  quit that aterm, launch /Applications/aterm.app, then cut again",
        named.join("; ")
    )))
}

/// The ledger the release's engine at `copy` committed (`HEAD:mappings.json`) where it holds a
/// ledger commit its origin's main lacks, as it last fetched it: a row `pub publish` recorded and
/// could not push, which only that engine holds until its next sync delivers it
/// ([`ledger_with_unpushed`]). `None` where it holds none, or is no checkout. Read by
/// [`crate::ledger::object_git`], as [`crate::github_remote::origin_slug`] judges the copy's
/// repository: an inherited `GIT_DIR` or a host's configuration cannot answer for another
/// repository's rows.
pub fn unpushed_ledger(copy: &Path, run: aterm_verify::engine::RunGit<'_>) -> Option<String> {
    if !copy.join(".git").exists() {
        return None;
    }
    let git = |args: &[&str]| {
        let mut cmd = crate::ledger::object_git(copy).ok()?;
        cmd.args(args);
        run(&mut cmd)
            .ok()
            .filter(|out| out.status.success())
            .map(|out| out.stdout)
    };
    let ahead = git(&[
        "rev-list",
        "refs/remotes/origin/main..HEAD",
        "--",
        "mappings.json",
    ])?;
    if ahead.trim_ascii().is_empty() {
        return None;
    }
    String::from_utf8(git(&["show", "HEAD:mappings.json"])?).ok()
}

/// The ledger `origin` holds with each `mappings.<repo>.<commit>` row of `unpushed`
/// ([`unpushed_ledger`]) it lacks added: everything recorded, pushed or not. `origin` as it is
/// where either does not parse as a ledger.
#[must_use]
pub fn ledger_with_unpushed(origin: &str, unpushed: &str) -> String {
    use aterm_json::{Map, Value};
    let (Ok(mut whole), Ok(extra)) = (
        aterm_json::from_str::<Value>(origin),
        aterm_json::from_str::<Value>(unpushed),
    ) else {
        return origin.to_string();
    };
    let (Some(top), Some(extra)) = (
        whole.as_object_mut(),
        extra.get("mappings").and_then(Value::as_object),
    ) else {
        return origin.to_string();
    };
    let Some(mappings) = top
        .entry("mappings".to_string())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
    else {
        return origin.to_string();
    };
    for (repo, rows) in extra {
        let (Some(rows), Some(into)) = (
            rows.as_object(),
            mappings
                .entry(repo.clone())
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut(),
        ) else {
            continue;
        };
        for (commit, row) in rows {
            into.entry(commit.clone()).or_insert_with(|| row.clone());
        }
    }
    aterm_json::to_string(&whole).unwrap_or_else(|_| origin.to_string())
}

/// The ledger a reader of the clone at `repo` judges, `var` the environment, and where it was
/// read: `mappings.json` of one commit of the engine ([`aterm_verify::engine`]), never a working
/// tree. `PUBLICATION_ENGINE` names the engine a release hands each child (its own, synced, every
/// row it recorded committed there), read at its HEAD; else the engine is the main of the origin
/// the clone's engine names (where a release made one; else `~/publication`), imported now
/// through `open`'s native authority for it ([`crate::github_remote::Remote::refresh_main`]),
/// since no checkout here is kept level with it, with the rows the clone's engine holds unpushed
/// ([`unpushed_ledger`]).
fn ledger_text(
    repo: &Path,
    var: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    open: &dyn Fn(&Path, &Path) -> std::result::Result<crate::github_remote::Remote, String>,
) -> Result<(String, String)> {
    use aterm_verify::engine::{self, Commit, Named};
    let refused = |named: Option<&Named>| {
        let fix = match named {
            Some(Named::Checkout(_)) => {
                "PUBLICATION_ENGINE names the engine whose ledger is read at its HEAD: the one \
                 `ship release` synced and published through, which it hands the cut; unset it to \
                 read the ledger on the engine's origin"
            }
            _ => {
                "clone the engine to ~/publication, or point PUBLICATION_ENGINE at the engine \
                 that published"
            }
        };
        move |e: engine::Error| {
            Error::new(format!(
                "cannot read the publication ledger: {e} — a cut releases the newest commit `pub \
                 publish` recorded on the engine's main; {fix}. Nothing was claimed."
            ))
        }
    };
    let named = engine::named(repo, var).map_err(refused(None))?;
    let run: engine::RunGit<'_> = &engine::bounded;
    let import = |at: &Path, by: &Path| open(at, by)?.refresh_main();
    let commit = Commit::read(&named, run, &import).map_err(refused(Some(&named)))?;
    let text = commit
        .text("mappings.json", run)
        .map_err(refused(Some(&named)))?;
    let read = format!("mappings.json on {}", commit.shown());
    let Named::Origin { by, .. } = &named else {
        return Ok((text, read));
    };
    let copy = engine::engine_copy(&engine::release_dir(repo));
    // The copy's unpushed rows are its own origin's records: they count only where that is the
    // origin read. A copy whose origin URL is gone while its remote-tracking refs stay (its
    // remote section removed or its URL unset by hand; `git remote remove` deletes the refs too)
    // keeps the main it last fetched, and `~/publication` may name another repository.
    let unpushed = (*by == copy).then(|| unpushed_ledger(&copy, run)).flatten();
    Ok(match unpushed {
        Some(unpushed) => (
            ledger_with_unpushed(&text, &unpushed),
            format!("{read}, with the rows {} holds unpushed", copy.display()),
        ),
        None => (text, read),
    })
}

/// The dev commit the newest aterm source publication exported, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedSource {
    /// Full dev sha — the `mappings.aterm.<sha>` key.
    pub commit: String,
    /// The row's `verified_at`, verbatim (RFC 3339).
    pub verified_at: String,
}

/// Parses an RFC 3339 timestamp into `(utc_seconds, nanoseconds)` so rows with mixed UTC
/// offsets or fractional seconds compare chronologically rather than lexicographically.
fn rfc3339_instant(s: &str) -> Option<(i64, u32)> {
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let digits = |slice: &[u8]| -> Option<i64> {
        let mut v = 0_i64;
        for &d in slice {
            if !d.is_ascii_digit() {
                return None;
            }
            v = v * 10 + i64::from(d - b'0');
        }
        Some(v)
    };
    let (y, mo, d) = (digits(&b[0..4])?, digits(&b[5..7])?, digits(&b[8..10])?);
    let (h, mi, se) = (
        digits(&b[11..13])?,
        digits(&b[14..16])?,
        digits(&b[17..19])?,
    );
    let max_day = match mo {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => return None,
    };
    if !(1..=max_day).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let rest = &b[19..];
    let (nanos, tz) = if let Some(after_dot) = rest.strip_prefix(b".") {
        let n_digits = after_dot.iter().take_while(|c| c.is_ascii_digit()).count();
        if n_digits == 0 {
            return None;
        }
        let mut ns = 0_u32;
        for i in 0..9 {
            let digit = after_dot
                .get(i)
                .copied()
                .filter(|_| i < n_digits)
                .unwrap_or(b'0');
            ns = ns * 10 + u32::from(digit - b'0');
        }
        (ns, &after_dot[n_digits..])
    } else {
        (0_u32, rest)
    };
    let offset_secs = match tz {
        [b'Z' | b'z'] => 0_i64,
        [sign @ (b'+' | b'-'), h0, h1, b':', m0, m1] => {
            let (oh, om) = (digits(&[*h0, *h1])?, digits(&[*m0, *m1])?);
            if oh > 23 || om > 59 {
                return None;
            }
            let mag = oh * 3600 + om * 60;
            if *sign == b'-' { -mag } else { mag }
        }
        _ => return None,
    };
    let civil_secs =
        aterm_types::rfc3339::days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se.min(59);
    let extra_leap = u32::from(se == 60) * 1_000_000_000;
    Some((civil_secs - offset_secs, nanos.saturating_add(extra_leap)))
}

/// The rows of `repo` the engine's ledger `value` revoked (`revoked.<repo>`, keyed by source
/// commit): a revoked publication is not one any reader counts, as the engine's own projection
/// counts none (`bin/pub_projection.py`'s `live_rows`).
#[must_use]
pub fn revoked_rows<'a>(value: &'a aterm_json::Value, repo: &str) -> Option<&'a aterm_json::Map> {
    value
        .get("revoked")
        .and_then(|r| r.get(repo))
        .and_then(aterm_json::Value::as_object)
}

/// The newest live `mappings.aterm` row by `verified_at` in the engine's ledger text, every
/// revoked row ([`revoked_rows`]) left out.
///
/// # Errors
/// Unparseable JSON, no `mappings.aterm` object, or no live row with a full-hex key.
pub fn newest_published_source(mappings_json: &str) -> Result<PublishedSource> {
    let value: aterm_json::Value = aterm_json::from_str(mappings_json)
        .map_err(|e| Error::new(format!("the publication ledger is not JSON: {e}")))?;
    let rows = value
        .get("mappings")
        .and_then(|m| m.get("aterm"))
        .and_then(aterm_json::Value::as_object)
        .ok_or_else(|| Error::new("the publication ledger has no `mappings.aterm` rows"))?;
    let revoked = revoked_rows(&value, "aterm");
    rows.iter()
        .filter(|(sha, _)| aterm_codec::hex::is_hex(sha, 40))
        .filter(|(sha, _)| !revoked.is_some_and(|r| r.contains_key(*sha)))
        .map(|(sha, row)| {
            let verified_at = row
                .get("verified_at")
                .and_then(aterm_json::Value::as_str)
                .unwrap_or_default();
            (sha.as_str(), verified_at)
        })
        .max_by_key(|&(sha, verified_at)| (rfc3339_instant(verified_at), verified_at, sha))
        .map(|(commit, verified_at)| PublishedSource {
            commit: commit.to_string(),
            verified_at: verified_at.to_string(),
        })
        .ok_or_else(|| Error::new("the publication ledger has no live aterm row with a commit key"))
}

pub(crate) fn read_bounded_bytes(path: &Path, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let (file, meta) = aterm_uds::regular_file::open(path)?;
    if meta.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds size limit",
        ));
    }
    let cap = usize::try_from(meta.len().min(max_bytes))
        .unwrap_or(0)
        .saturating_add(1);
    let mut bytes = Vec::with_capacity(cap);
    file.take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds size limit",
        ));
    }
    Ok(bytes)
}

pub(crate) fn read_bounded_string(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    use std::io::Read as _;
    let (file, meta) = aterm_uds::regular_file::open(path)?;
    if meta.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds size limit",
        ));
    }
    let cap = usize::try_from(meta.len().min(max_bytes))
        .unwrap_or(0)
        .saturating_add(1);
    let mut text = String::with_capacity(cap);
    file.take(max_bytes + 1).read_to_string(&mut text)?;
    if text.len() as u64 > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds size limit",
        ));
    }
    Ok(text)
}

/// The commit a real cut of the clone at `repo` releases: the newest aterm row of the ledger
/// [`ledger_text`] reads.
///
/// # Errors
/// An unreadable or rowless ledger is a refusal.
pub fn published_source(repo: &Path) -> Result<PublishedSource> {
    published_source_in(repo, &|name| env::var_os(name), &|at, _| {
        crate::github_remote::Remote::origin(at)
    })
}

fn published_source_in(
    repo: &Path,
    var: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    open: &dyn Fn(&Path, &Path) -> std::result::Result<crate::github_remote::Remote, String>,
) -> Result<PublishedSource> {
    let (text, read) = ledger_text(repo, var, open)?;
    newest_published_source(&text).map_err(|e| Error::new(format!("{read}: {e}")))
}

/// Where [`place_published`] put the commit a real cut releases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedCheckout {
    /// The published commit, now the cut tree's `HEAD` (detached).
    pub source: PublishedSource,
    /// The cut tree ([`cut_tree_path`]) — the checkout the cut reads and builds.
    pub tree: PathBuf,
    /// Whether the cut tree had to be created or moved to get there.
    pub moved: bool,
    /// How many commits `origin/main` carries past it — none of which this cut ships.
    pub main_ahead: u64,
}

/// Put the cut tree ([`cut_tree_path`]) at `source` detached so the cut reads and builds the exact exported commit.
///
/// # Errors
/// Each precondition above, [`place_cut_tree`]'s, and any git failure.
pub fn place_published(
    git: &dyn GitRunner,
    origin: &dyn crate::ledger::Origin,
    repo: &Path,
    source: &PublishedSource,
) -> Result<PublishedCheckout> {
    let main = origin
        .refresh_main()
        .map_err(|e| Error::new(format!("cannot reach origin (no offline cuts): {e}")))?;
    let commit = source.commit.as_str();
    if !git
        .git(&["cat-file", "-e", &format!("{commit}^{{commit}}")])?
        .success()
    {
        return Err(Error::new(format!(
            "the published commit {commit} (verified {}) is not in this repository even \
             after fetching origin/main — PUBLICATION_ENGINE names an engine whose aterm \
             rows are not this repository's history. Nothing was claimed.",
            source.verified_at
        )));
    }
    let on_main = git.git(&["merge-base", "--is-ancestor", commit, &main])?;
    match on_main.status {
        0 => {}
        1 => {
            return Err(Error::new(format!(
                "the published commit {commit} (verified {}) is not on origin/main — `pub \
                 stage` exports main's history and the release claim lands on main, so a \
                 release cannot be cut from a commit main does not carry. Nothing was claimed.",
                source.verified_at
            )));
        }
        _ => {
            return Err(Error::new(format!(
                "cannot tell whether the published commit {commit} is on origin/main \
                 (git merge-base --is-ancestor failed: {})",
                on_main.stderr_utf8().trim()
            )));
        }
    }
    let range = format!("{commit}..{main}");
    let count_out = git_ok(git, &["rev-list", "--count", &range])?.stdout_utf8();
    let count_trimmed = count_out.trim();
    if !count_trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::new(format!(
            "git rev-list --count {range}: invalid count {count_trimmed:?}"
        )));
    }
    let main_ahead = count_trimmed
        .parse::<u64>()
        .map_err(|e| Error::new(format!("git rev-list --count {range}: {e}")))?;
    let tree = place_cut_tree(git, repo, commit)?;
    Ok(PublishedCheckout {
        source: source.clone(),
        tree: tree.path,
        moved: tree.moved,
        main_ahead,
    })
}

/// The cut tree for the checkout at `repo`.
///
/// # Errors
/// A checkout at the filesystem root has no parent to hold the quiet folder.
pub fn cut_tree_path(repo: &Path) -> Result<PathBuf> {
    match (repo.parent(), repo.file_name()) {
        (Some(parent), Some(name)) => {
            let mut leaf = name.to_os_string();
            leaf.push("-cut.noindex");
            Ok(parent.join(aterm_tempfile::QUIET).join(leaf))
        }
        _ => Err(Error::new(format!(
            "{} has no parent directory to hold the cut tree's quiet folder",
            repo.display()
        ))),
    }
}

/// Where [`place_cut_tree`] left the cut tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutTree {
    /// [`cut_tree_path`] of the checkout.
    pub path: PathBuf,
    /// Whether it had to be created or moved.
    pub moved: bool,
}

/// Put the cut tree ([`cut_tree_path`]) at `commit`, detached, creating it as a linked worktree of `repo`'s repository when absent.
///
/// # Errors
/// Non-worktree path, dirty cut tree, rewriting `info/attributes` ([`refuse_rewriting_info_attributes`]), or git failure.
pub fn place_cut_tree(git: &dyn GitRunner, repo: &Path, commit: &str) -> Result<CutTree> {
    refuse_rewriting_info_attributes(git)?;
    let path = cut_tree_path(repo)?;
    if !path.exists() {
        let parent = path
            .parent()
            .ok_or_else(|| Error::new("cut tree has no parent"))?;
        // The shared /tmp/.ubd is allowed by quiet_in's owner/sticky checks; every other
        // newly named parent must satisfy the release's private-directory rule.
        if aterm_tempfile::quiet_in(
            repo.parent()
                .ok_or_else(|| Error::new("checkout has no parent"))?,
        ) != parent
        {
            crate::snapshot::private_dir(parent, "cut tree parent").map_err(Error::new)?;
        }
        git_ok(git, &["worktree", "prune"])?;
        let spelled = path.to_string_lossy().into_owned();
        let add = ["worktree", "add", "-q", "--detach", &spelled, commit];
        git_ok(git, &[&BYTE_EXACT[..], &add].concat()).map_err(|e| {
            Error::new(format!(
                "cannot create the cut tree {}: {e}. Nothing was claimed.",
                path.display()
            ))
        })?;
        let path = checked_head(&path, commit)?;
        align_submodules(&crate::ledger::GitCli::new(&path))?;
        return Ok(CutTree { path, moved: true });
    }
    let common = |dir: &Path| -> Option<PathBuf> {
        let out = crate::ledger::git_command()
            .ok()?
            .arg("-C")
            .arg(dir)
            .args([
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
                "--show-toplevel",
            ])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8(out.stdout).ok()?;
        let mut lines = text.lines();
        let common = fs::canonicalize(lines.next()?).ok()?;
        let top = fs::canonicalize(lines.next()?).ok()?;
        (top == fs::canonicalize(dir).ok()?).then_some(common)
    };
    match (common(repo), common(&path)) {
        (Some(ours), Some(theirs)) if ours == theirs => {}
        _ => {
            return Err(Error::new(format!(
                "{} exists and is not a worktree of this repository — it is where the cut \
                 tree goes, and the cutter does not touch what it did not make. Move it \
                 away, then cut again. Nothing was claimed.",
                path.display()
            )));
        }
    }
    let tree = crate::ledger::GitCli::new(&path);
    require_clean_cut_tree(&path)?;
    let moved = rev_parse(&tree, "HEAD")? != commit;
    if moved {
        let checkout = [
            "checkout",
            "-q",
            "--detach",
            "--no-overwrite-ignore",
            commit,
        ];
        git_ok(&tree, &[&BYTE_EXACT[..], &checkout].concat())?;
    }
    let path = checked_head(&path, commit)?;
    align_submodules(&tree)?;
    require_clean_cut_tree(&path)?;
    Ok(CutTree { path, moved })
}

const REWRITING_ATTRIBUTES: [&str; 6] = [
    "text",
    "eol",
    "crlf",
    "filter",
    "ident",
    "working-tree-encoding",
];

fn refuse_rewriting_info_attributes(git: &dyn GitRunner) -> Result<()> {
    let at = git_ok(
        git,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "info/attributes",
        ],
    )?
    .stdout_utf8();
    let path = PathBuf::from(at.trim_end_matches('\n'));
    let text = match read_bounded_string(&path, 16 << 20) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(Error::new(format!(
                "cannot read repository attributes {}: {error}. The cut cannot establish \
                 the commit's bytes until those attributes can be checked. Nothing was claimed.",
                path.display()
            )));
        }
    };
    let rewriting = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .find(|line| {
            line.split_whitespace().skip(1).any(|attr| {
                let name = attr.trim_start_matches(['-', '!']);
                let name = name.split_once('=').map_or(name, |(name, _)| name);
                REWRITING_ATTRIBUTES.contains(&name)
            })
        });
    match rewriting {
        Some(line) => Err(Error::new(format!(
            "{} says `{line}`, which rewrites the bytes a checkout writes and outranks every \
             setting the release passes, so the cut tree would not hold the commit's bytes. \
             Move that line out of the file, then cut again. Nothing was claimed.",
            path.display()
        ))),
        None => Ok(()),
    }
}

fn require_clean_cut_tree(path: &Path) -> Result<()> {
    let changes: Vec<String> = crate::clean::dirty_against_head(path)
        .ok_or_else(|| {
            Error::new(format!(
                "git could not list the state of the cut tree {}. Nothing was claimed.",
                path.display()
            ))
        })?
        .into_iter()
        .filter(|(_, dirt)| *dirt != crate::clean::Dirt::Unpopulated)
        .map(|(p, _)| p)
        .collect();
    if !changes.is_empty() {
        return Err(Error::new(format!(
            "the cut tree {} has changes — it belongs to the cutter, which never discards \
             work it did not make:\n  {}\nPreserve or resolve the listed files, then cut \
             again. Nothing was claimed.",
            path.display(),
            changes
                .iter()
                .take(5)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("\n  ")
        )));
    }
    Ok(())
}

fn checked_head(path: &Path, commit: &str) -> Result<PathBuf> {
    let now = rev_parse(&crate::ledger::GitCli::new(path), "HEAD")?;
    if now != commit {
        return Err(Error::new(format!(
            "placing the cut tree {} at {commit} left its HEAD at {now} — refusing to cut \
             from a tree that will not move",
            path.display()
        )));
    }
    Ok(path.to_path_buf())
}

/// Where the whole-tree Trust lane files its facts.
pub(crate) fn receipt_store(git: &dyn GitRunner) -> Result<std::path::PathBuf> {
    let common = git_ok(
        git,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?
    .stdout_utf8();
    Ok(std::path::Path::new(common.trim()).join("aterm-verify/receipts"))
}

fn short(sha: &str) -> &str {
    sha.get(..9).unwrap_or(sha)
}

/// This process's pinned Trust toolchain, used by release builds and the default explicit Trust verdict.
///
/// # Errors
/// No toolchain, or a trustc that does not answer `-vV`.
pub fn cut_toolchain() -> Result<String> {
    toolchain_at(&trust_stage2_bin()?)
}

/// The `trustc` commit-hash a [`cut_toolchain`] identity names (`unknown` when it names none).
#[must_use]
pub fn toolchain_trustc(toolchain: &str) -> &str {
    toolchain
        .rsplit_once(" trustc ")
        .map_or("unknown", |(_, commit)| commit)
}

/// [`cut_toolchain`]'s spelling for the toolchain whose `bin/` is `stage2`.
///
/// # Errors
/// No such directory, or a trustc there that does not answer `-vV`.
pub fn toolchain_at(stage2: &Path) -> Result<String> {
    let stage2 =
        fs::canonicalize(stage2).map_err(|e| Error::new(format!("{}: {e}", stage2.display())))?;
    let trustc = stage2.join("trustc");
    let out = command(&trustc).arg("-vV").output().map_err(|e| {
        Error::new(format!(
            "trustc at {} did not run -vV: {e}",
            trustc.display()
        ))
    })?;
    if !out.status.success() {
        return Err(Error::new(format!(
            "trustc at {} failed -vV: {}",
            trustc.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let commit = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("commit-hash:"))
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .unwrap_or("unknown");
    Ok(format!("{} trustc {commit}", stage2.display()))
}

/// Did these bytes, from this tree, pass every check a release requires, each by its own record
/// under the current build-inputs seal ([`crate::check_record::checks`])? Its words name the
/// missing and the failed checks.
///
/// # Errors
/// git cannot name the tree, a record cannot be read at all, or the seal cannot be taken: no
/// answer either way.
pub fn verified(git: &dyn GitRunner, commit: &str, macho_sha256: &str) -> Result<(bool, String)> {
    verified_with_inputs(git, commit, macho_sha256, crate::build_inputs::current)
}

pub(crate) fn verified_with_inputs(
    git: &dyn GitRunner,
    commit: &str,
    macho_sha256: &str,
    inputs: impl FnOnce() -> std::result::Result<String, String>,
) -> Result<(bool, String)> {
    let checks =
        crate::check_record::checks(git, commit, Some(macho_sha256), inputs).map_err(Error::new)?;
    Ok((checks.complete(), checks.line().to_owned()))
}

/// `ship check`'s `verify release` line for `commit`.
///
/// # Errors
/// Not verified, or not built yet, with the remedy; and a record or journal that cannot be read.
pub fn verify_gate_at(git: &dyn GitRunner, commit: &str) -> Result<String> {
    verify_gate_at_with_inputs(git, commit, crate::build_inputs::current)
}

fn verify_gate_at_with_inputs(
    git: &dyn GitRunner,
    commit: &str,
    inputs: impl FnOnce() -> std::result::Result<String, String>,
) -> Result<String> {
    let journal = crate::release::Journal::load(
        &crate::release::release_dir(git)
            .map_err(Error::new)?
            .join(crate::release::JOURNAL_FILE),
    )
    .map_err(Error::new)?;
    let Some(artifact) = journal
        .filter(|j| j.commit == commit)
        .and_then(|j| j.artifact)
    else {
        return Err(Error::new(format!(
            "no bundle of {} is built in this clone yet (its release journal names none) — \
             `{}` builds it once and `verify release` judges those bytes; nothing is signed \
             unverified",
            short(commit),
            crate::release::RELEASE_COMMAND
        )));
    };
    match verified_with_inputs(git, commit, &artifact.macho_sha256, inputs)? {
        (true, line) => Ok(line),
        (false, why) => Err(Error::new(format!(
            "the bundle of {} (build {}) has not passed `verify release` ({why}) — `{}` runs \
             it: `{}` in the clone",
            short(commit),
            artifact.build,
            crate::release::RELEASE_COMMAND,
            crate::release::verify_command(&artifact.app.display().to_string())
        ))),
    }
}

/// What one `ship check` gate answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It ran and passed: its transcript line.
    Pass(String),
    /// It ran, and a release of the commit would be refused: why, and the remedy.
    Refuse(Refusal),
    /// It decided nothing (a missing tool, an engine that is not current, another commit's cutter).
    CouldNotRun(Refusal),
}

/// A cut gate's error text as a [`Refusal`].
#[must_use]
pub fn gate_refusal(text: &str) -> Refusal {
    Refusal::new(
        text.lines().next().unwrap_or(""),
        text.lines()
            .find_map(|l| l.trim().strip_prefix("fix:"))
            .map(str::trim)
            .unwrap_or(""),
    )
    .detail(
        text.lines()
            .skip(1)
            .filter(|l| !l.trim().starts_with("fix:"))
            .map(str::to_string),
    )
}

/// One gate `ship check` ran: its name and its [`Outcome`].
#[derive(Debug)]
pub struct PrecheckGate {
    pub name: &'static str,
    pub outcome: Outcome,
}

/// What `ship check` found for one commit.
#[derive(Debug)]
pub struct Precheck {
    /// The commit judged, full.
    pub commit: String,
    /// Its `[workspace.package]` version: the release a cut of it would be.
    pub version: String,
    /// Every gate asked, in order.
    pub gates: Vec<PrecheckGate>,
}

impl Precheck {
    /// The commit `rev` names and the release its `Cargo.toml` declares, no gate asked yet.
    ///
    /// # Errors
    /// `rev` names no commit, or its `Cargo.toml` carries no release version.
    pub fn of(git: &dyn GitRunner, rev: &str) -> Result<Self> {
        let commit = rev_parse(git, &format!("{rev}^{{commit}}"))?;
        let manifest = git_ok(git, &["cat-file", "blob", &format!("{commit}:Cargo.toml")])?;
        let version = crate::publish::release_version_from_workspace(
            &crate::publish::workspace_version(&manifest.stdout_utf8())?,
        )?;
        Ok(Self {
            commit,
            version,
            gates: Vec::new(),
        })
    }

    /// The gates that would refuse a release of [`Self::commit`], in order.
    #[must_use]
    pub fn refused(&self) -> Vec<&'static str> {
        self.named(|o| matches!(o, Outcome::Refuse(_)))
    }

    /// The gates that could not run, in order.
    #[must_use]
    pub fn not_run(&self) -> Vec<&'static str> {
        self.named(|o| matches!(o, Outcome::CouldNotRun(_)))
    }

    fn named(&self, pick: impl Fn(&Outcome) -> bool) -> Vec<&'static str> {
        self.gates
            .iter()
            .filter(|g| pick(&g.outcome))
            .map(|g| g.name)
            .collect()
    }

    /// `ship check`'s exit: 0 every gate passed, 1 one would refuse, 3 none refused but one could not run.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        if self
            .gates
            .iter()
            .any(|g| matches!(g.outcome, Outcome::Refuse(_)))
        {
            1
        } else if self
            .gates
            .iter()
            .any(|g| matches!(g.outcome, Outcome::CouldNotRun(_)))
        {
            3
        } else {
            0
        }
    }

    /// The transcript's first line.
    #[must_use]
    pub fn header(&self) -> String {
        format!(
            "aterm-release · check {} (v{}): would a release of it pass?\n",
            short(&self.commit),
            self.version
        )
    }

    /// One gate's lines: `ok` with its line, or `REFUSE` / `NOT RUN` with why, the detail and the fix.
    #[must_use]
    pub fn gate_lines(gate: &PrecheckGate) -> String {
        use std::fmt::Write as _;
        let (word, r) = match &gate.outcome {
            Outcome::Pass(line) => return format!("  ok      {line}\n"),
            Outcome::Refuse(r) => ("REFUSE ", r),
            Outcome::CouldNotRun(r) => ("NOT RUN", r),
        };
        let mut out = format!(
            "  {word} {} — {}\n",
            gate.name,
            r.why.replace('\n', "\n          ")
        );
        for line in &r.detail {
            let _ = writeln!(out, "          {line}");
        }
        if !r.remedy.is_empty() {
            let _ = writeln!(out, "          fix: {}", r.remedy);
        }
        out
    }

    /// What `ship check` prints before its verdict: the header, then each gate.
    #[cfg(test)]
    #[must_use]
    pub fn transcript(&self) -> String {
        let mut out = self.header();
        for gate in &self.gates {
            out.push_str(&Self::gate_lines(gate));
        }
        out
    }

    /// The verdict line, for [`Self::exit_code`].
    #[must_use]
    pub fn verdict(&self) -> String {
        let (commit, version) = (short(&self.commit), &self.version);
        match self.exit_code() {
            0 => format!(
                "READY — {commit} (v{version}) passes these gates; the cut asks the gates of \
                 its own moment (the tag, the channel, the lock, the roster, the credentials) \
                 itself"
            ),
            1 => format!(
                "REFUSED — a release of {commit} (v{version}) would refuse at: {}; do not \
                 publish its source until it passes",
                self.refused().join(", ")
            ),
            _ => format!(
                "NOT ANSWERED — {} could not run for {commit} (v{version}); fix each (its fix \
                 line above) and run the check again",
                self.not_run().join(", ")
            ),
        }
    }
}

/// `ship check --cut` (and a bare `ship check`).
///
/// # Errors
/// `rev` names no commit, or the commit's Cargo.toml carries no release version.
pub fn precheck(
    git: &dyn GitRunner,
    origin: Result<&dyn crate::ledger::Origin>,
    rev: &str,
) -> Result<Precheck> {
    precheck_with(git, origin, rev, crate::cutter::running())
}

/// Source-only main sampling asks every commit gate and makes no claim about bundle bytes.
/// # Errors
/// The revision cannot be read or names no release version.
pub fn precheck_source(
    git: &dyn GitRunner,
    origin: Result<&dyn crate::ledger::Origin>,
    rev: &str,
) -> Result<Precheck> {
    precheck_parts(
        git,
        origin,
        rev,
        crate::cutter::running(),
        None::<fn(&str) -> Result<String>>,
    )
}

fn precheck_with(
    git: &dyn GitRunner,
    origin: Result<&dyn crate::ledger::Origin>,
    rev: &str,
    stamp: std::result::Result<&str, &crate::cutter::NotOwn>,
) -> Result<Precheck> {
    precheck_with_judge(git, origin, rev, stamp, |commit| {
        verify_gate_at(git, commit)
    })
}

fn precheck_with_judge(
    git: &dyn GitRunner,
    origin: Result<&dyn crate::ledger::Origin>,
    rev: &str,
    stamp: std::result::Result<&str, &crate::cutter::NotOwn>,
    verify: impl FnOnce(&str) -> Result<String>,
) -> Result<Precheck> {
    precheck_parts(git, origin, rev, stamp, Some(verify))
}

fn precheck_parts(
    git: &dyn GitRunner,
    origin: Result<&dyn crate::ledger::Origin>,
    rev: &str,
    stamp: std::result::Result<&str, &crate::cutter::NotOwn>,
    verify: Option<impl FnOnce(&str) -> Result<String>>,
) -> Result<Precheck> {
    let mut found = Precheck::of(git, rev)?;
    let (commit, version) = (found.commit.clone(), found.version.clone());
    let identity = match own_cutter_of(git, stamp, &commit) {
        Ok(note) => note,
        Err(not) => {
            let fix = if stamp.is_ok() {
                format!(
                    "ask with the commit's own cutter — `PUB_SOURCE_COMMIT={commit} \
                     publish/pre-promote` builds it in a throwaway worktree of the commit and \
                     runs the check there (what `pub publish` runs), or run `targo \
                     --unverified ship check` in a clean checkout of that commit"
                )
            } else {
                not.fix
            };
            found.gates.push(PrecheckGate {
                name: "cutter identity",
                outcome: Outcome::CouldNotRun(Refusal::new(
                    format!(
                        "{} — the cut's gates are not asked: they would be another tree's \
                         rules, not the ones a cut of {} runs",
                        not.why,
                        short(&commit)
                    ),
                    fix,
                )),
            });
            return Ok(found);
        }
    };
    let with_origin = |check: &dyn Fn(&dyn crate::ledger::Origin) -> Result<String>| match &origin {
        Ok(origin) => check(*origin),
        Err(error) => Err(Error::new(error.to_string())),
    };
    let answer = |outcome: Result<String>| match outcome {
        Ok(line) => Outcome::Pass(line),
        Err(e) => Outcome::Refuse(gate_refusal(&e.to_string())),
    };
    found.gates = vec![
        PrecheckGate {
            name: "cutter identity",
            outcome: Outcome::Pass(identity),
        },
        PrecheckGate {
            name: "changelog",
            outcome: answer(with_origin(&|origin| {
                let catalog = origin.tag_catalog().map_err(Error::new)?;
                changelog_gate_at(git, &commit, &version, &catalog).map(|(section, found)| {
                    format!("CHANGELOG [{section}]: {} entries, no '''", found.entries)
                })
            })),
        },
        PrecheckGate {
            name: "handoff fixtures",
            outcome: answer(with_origin(&|origin| {
                handoff_fixture_gate_at(git, origin, &commit, &version)
                    .map(|found| handoff_fixtures_line(found.as_ref()))
            })),
        },
        PrecheckGate {
            name: "harness versions",
            outcome: answer(with_origin(&|origin| {
                harness_versions_gate_at(git, origin, &commit, &version)
            })),
        },
        PrecheckGate {
            name: "handoff policy",
            outcome: answer(with_origin(&|origin| {
                handoff_policy_gate_at(git, origin, &commit, &version)
                    .map(|found| format!("handoff policy a cut seals into the bundle: {found}"))
            })),
        },
    ];
    if let Some(verify) = verify {
        found.gates.push(PrecheckGate {
            name: "verify release",
            outcome: answer(verify(&commit)),
        });
    }
    Ok(found)
}

/// The harness versions gate ([`crate::harness_versions`]) for `commit`: every harness whose
/// sources moved since the previous public release carries a bumped `HARNESS_VER`.
///
/// # Errors
/// A harness owes a bump, or as [`harness_versions_base`].
pub fn harness_versions_gate_at(
    git: &dyn GitRunner,
    origin: &dyn crate::ledger::Origin,
    commit: &str,
    version: &str,
) -> Result<String> {
    match harness_versions_base(git, origin, commit, version)? {
        None => Ok("harness versions: no earlier public release to compare".to_string()),
        Some((release, released)) => {
            crate::harness_versions::notes_gate_at(git, (&release, &released), commit, version)?;
            crate::harness_versions::gate(git, (&release, &released), commit)
        }
    }
}

/// The harness question of a bump still to be written, asked of its base `commit`, which
/// declares `from`. The bump commit writes every harness version the window owes
/// ([`crate::harness_versions::apply`], `ship harness-bump`'s writes), so what is owed is never
/// refused: it is returned as the line naming each version that commit writes, for the
/// launcher to say; `None` where nothing is owed. The window's notes are judged as
/// [`harness_versions_gate_at`] judges them. A tree that already declares another version (the
/// pushed bump, or a dry run's private bump, which carries the writes) is held to
/// [`harness_versions_gate_at`] itself.
///
/// # Errors
/// As [`harness_versions_gate_at`]; also when `commit`'s Cargo.toml names no
/// `[workspace.package]` version.
pub fn harness_versions_before_bump_at(
    git: &dyn GitRunner,
    origin: &dyn crate::ledger::Origin,
    commit: &str,
    from: &str,
    version: &str,
) -> Result<Option<String>> {
    let declared =
        crate::publish::workspace_version(&GateTree::Commit { git, commit }.text("Cargo.toml")?)?;
    if declared != from {
        return harness_versions_gate_at(git, origin, commit, version).map(|_| None);
    }
    let Some((release, released)) = harness_versions_base(git, origin, commit, version)? else {
        return Ok(None);
    };
    crate::harness_versions::notes_gate_at(git, (&release, &released), commit, version)?;
    let owed = crate::harness_versions::owed_between(git, &released, commit)?;
    if owed.is_empty() {
        return crate::harness_versions::gate(git, (&release, &released), commit).map(|_| None);
    }
    let names: Vec<String> = owed
        .iter()
        .map(|o| format!("alab {} harness {}.{}", o.harness.cli(), o.to.0, o.to.1))
        .collect();
    Ok(Some(format!(
        "harness versions since v{release}: the bump commit writes {}",
        names.join(", ")
    )))
}

/// The public release that opened release `version`'s harness window, and its commit, as
/// `commit`'s ledger and origin's tags name them; `None` where none precedes `version`.
///
/// # Errors
/// The ledger or origin's tags cannot be read, the ledger names earlier releases none of which
/// shipped, or the previous release's commit is not in this clone.
fn harness_versions_base(
    git: &dyn GitRunner,
    origin: &dyn crate::ledger::Origin,
    commit: &str,
    version: &str,
) -> Result<Option<(String, String)>> {
    let ledger_text = GateTree::Commit { git, commit }.text(crate::ledger::LEDGER_FILE)?;
    let listing = origin.tag_catalog().map_err(|e| {
        Error::new(format!(
            "harness versions gate: cannot list origin's tags, which name the previous public \
             release: {e}"
        ))
    })?;
    let Some((release, released)) =
        crate::harness_versions::previous_release(&ledger_text, version, &listing)?
    else {
        return Ok(None);
    };
    if !git
        .git(&["cat-file", "-e", &format!("{released}^{{commit}}")])?
        .success()
    {
        return Err(Error::new(format!(
            "harness versions gate: v{release}'s commit {} is not in this clone — `git fetch \
             origin tag v{release}`, then check again",
            short(&released)
        )));
    }
    Ok(Some((release, released)))
}

/// The line for what [`handoff_fixture_gate_at`] found.
#[must_use]
pub fn handoff_fixtures_line(found: Option<&HandoffFixtures>) -> String {
    match found {
        Some(f) => format!(
            "handoff fixtures of v{} checked in: {} desks, {} pinned",
            f.release,
            f.desks.len(),
            f.pinned.len()
        ),
        None => "handoff fixtures: the ledger records no earlier release".to_string(),
    }
}

/// Prove the public channel's source tree already carries the version being cut.
pub fn channel_version_gate(repo: &Path, version: &str, offline: bool) -> Result<Option<String>> {
    let local = read_bounded_string(&repo.join("Cargo.toml"), 16 << 20)
        .map_err(|e| Error::new(format!("cannot read workspace Cargo.toml: {e}")))?;
    let Some(slug) = channel::update_channel_slug(&local)? else {
        return Ok(None);
    };
    if offline {
        return Ok(None);
    }

    let out = channel_api(&format!("repos/{slug}/contents/Cargo.toml?ref=main"))?;

    if out.status != 200 {
        if out.status != 404 {
            return Err(Error::new(format!(
                "cannot read Cargo.toml from the public channel {slug}: HTTP {}: {}",
                out.status,
                out.message()
            )));
        }
        let probe = channel_api(&format!("repos/{slug}"))?;
        if probe.status != 200 {
            return Err(Error::new(format!(
                "the public channel {slug} is not readable with the available credential, so \
                 this cut cannot confirm it carries v{version} (GitHub's 404 means missing \
                 or unauthorized).\n\
                 fix:  provide ~/.secrets/gh_access_token_alabsystems, or cut with --dry-run"
            )));
        }
        return Ok(None);
    }

    let body = String::from_utf8_lossy(&out.body).to_string();
    match channel::check_channel_version(version, &body)? {
        channel::ChannelVersion::Agrees => Ok(Some(version.to_string())),
        channel::ChannelVersion::NoManifest => Ok(None),
    }
}

fn channel_api(path: &str) -> Result<aterm_update_core::github_rest::Answer> {
    crate::github::raw(path)
        .map_err(|e| Error::new(format!("cannot read the public channel ({path}): {e}")))
}

/// Whether the lock itself was observed missing/changed, or metadata could not prove it.
#[derive(Debug)]
pub enum LockedMetadataError {
    /// Cargo.lock was observed missing or different from its prior bytes.
    Lockfile(String),
    /// The metadata check could not establish an exact offline resolution.
    Unavailable(String),
}

impl std::fmt::Display for LockedMetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lockfile(why) | Self::Unavailable(why) => f.write_str(why),
        }
    }
}

/// Prove the committed Cargo.lock resolves the workspace with no rewrite or network access.
pub fn locked_metadata_gate(repo: &Path) -> std::result::Result<(), LockedMetadataError> {
    locked_metadata_gate_with(repo, None)
}

fn locked_metadata_gate_with(
    repo: &Path,
    cargo_home: Option<&Path>,
) -> std::result::Result<(), LockedMetadataError> {
    use LockedMetadataError::{Lockfile, Unavailable};
    let lock_path = repo.join("Cargo.lock");
    let lock_before = read_bounded_bytes(&lock_path, 64 << 20).map_err(|error| {
        let why = format!("read committed Cargo.lock before release metadata check: {error}");
        if error.kind() == std::io::ErrorKind::NotFound {
            Lockfile(why)
        } else {
            Unavailable(why)
        }
    })?;
    let targo = resolve_targo().map_err(|error| Unavailable(error.to_string()))?;
    let mut metadata = command(&targo);
    metadata
        .args(["metadata", "--locked", "--offline", "--format-version", "1"])
        .current_dir(repo);
    if let Some(home) = cargo_home {
        metadata.env("CARGO_HOME", home);
    }
    let out = metadata
        .output()
        .map_err(|error| Unavailable(format!("failed to run locked targo metadata: {error}")))?;
    let lock_after = match read_bounded_bytes(&lock_path, 64 << 20) {
        Ok(bytes) => bytes,
        Err(read_error) if read_error.kind() == std::io::ErrorKind::NotFound => {
            crate::durable::write_fresh(&lock_path, &lock_before).map_err(|restore_error| {
                Lockfile(format!(
                    "Cargo.lock disappeared during locked metadata ({read_error}) and restoring \
                     its exact prior bytes failed: {restore_error}"
                ))
            })?;
            return Err(Lockfile(format!(
                "Cargo.lock disappeared during locked metadata ({read_error}); exact prior bytes \
                 were restored"
            )));
        }
        Err(read_error) => {
            return Err(Unavailable(format!(
                "read Cargo.lock after locked metadata: {read_error}"
            )));
        }
    };
    if lock_after != lock_before {
        crate::durable::write_fresh(&lock_path, &lock_before).map_err(|error| {
            Lockfile(format!(
                "locked Cargo metadata changed Cargo.lock and restoring its exact prior bytes \
                 failed: {error}"
            ))
        })?;
        return Err(Lockfile(
            "locked Cargo metadata attempted to rewrite Cargo.lock; exact prior bytes were \
             restored — refresh and commit the lock before cutting"
                .to_string(),
        ));
    }
    if !out.status.success() {
        return Err(Unavailable(format!(
            "locked offline Cargo metadata could not be verified; Cargo.lock is unchanged:\n{}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "gates_locked_metadata_tests.rs"]
mod locked_metadata_tests;

/// Releases are cut only on an arm64 Mac.
pub fn host_gate() -> Result<()> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok(())
    } else {
        Err(Error::new(
            "releases are cut only on a macOS arm64 host (codesign/hdiutil/lipo and the \
             Trust toolchain live there)"
                .to_string(),
        ))
    }
}

/// Clean tree: the release commit is exactly the published commit plus the changelog roll and ledger line, and the claim checks this tree out onto it.
pub fn clean_tree(git: &dyn GitRunner) -> Result<()> {
    let root = git_ok(git, &["rev-parse", "--show-toplevel"])?
        .stdout_utf8()
        .trim()
        .to_string();
    let dirty = dirty_paths(Path::new(&root))?;
    if dirty.is_empty() {
        return Ok(());
    }
    let mut lines: Vec<&str> = dirty.iter().take(5).map(String::as_str).collect();
    if dirty.len() > 5 {
        lines.push("…");
    }
    Err(Error::new(format!(
        "working tree has uncommitted changes — commit or remove them, or run from a clean \
         worktree:\n  {}",
        lines.join("\n  ")
    )))
}

/// The paths [`crate::clean`] finds dirty in the checkout at `root` against its `HEAD`.
///
/// # Errors
/// git could not list the checkout's state.
pub fn dirty_paths(root: &Path) -> Result<Vec<String>> {
    crate::clean::dirty_against_head(root)
        .map(|dirty| dirty.into_keys().collect())
        .ok_or_else(|| {
            Error::new(format!(
                "git could not list the state of {}",
                root.display()
            ))
        })
}

/// Every submodule initialised and checked out at exactly the commit `HEAD` records for it.
pub fn submodules_at_gitlinks(git: &dyn GitRunner) -> Result<()> {
    let out = git_ok(git, &["submodule", "status", "--recursive"])?;
    let text = out.stdout_utf8();
    let off = submodules_off_gitlink(&text);
    if off.is_empty() {
        return Ok(());
    }
    Err(Error::new(format!(
        "submodule(s) not checked out at the commit HEAD records — a cut here would build \
         a source HEAD does not name (`-` not initialised, `+` another commit, `U` \
         conflicted):\n  {}\nfix:  git submodule update --init --recursive   (astream is \
         private: it needs the same GitHub read access as aterm)",
        off.join("\n  ")
    )))
}

/// The `git submodule status` lines that are not at their gitlink.
pub fn submodules_off_gitlink(status: &str) -> Vec<&str> {
    status
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with(' '))
        .collect()
}

/// After [`place_cut_tree`] creates or moves the cut tree.
pub fn align_submodules(git: &dyn GitRunner) -> Result<()> {
    let update = ["submodule", "update", "--init", "--recursive", "--checkout"];
    // This graph includes private vendor/astream. An HTTPS spelling does not make a repository
    // credential-less; retain the ordinary credentialed Git path for the mixed submodule graph.
    let out = git.git(&[&BYTE_EXACT[..], &update].concat())?;
    if !out.success() {
        return Err(Error::new(format!(
            "the cut tree is at its commit, but its submodules could not be checked out at \
             the commits it records (exit {}): {}\nfix:  make each submodule's remote \
             readable here (astream is private: the same GitHub read access as aterm), then \
             cut again",
            out.status,
            out.stderr_utf8().trim()
        )));
    }
    submodules_at_gitlinks(git)
}

/// Every gitlink (`160000`) of an `ls-tree -r` listing: (rev, path).
#[must_use]
pub fn gitlinks(listing: &str) -> Vec<(String, String)> {
    listing
        .lines()
        .filter_map(|l| {
            let (meta, path) = l.split_once('\t')?;
            let mut f = meta.split_whitespace();
            (f.next()? == "160000").then(|| {
                let _kind = f.next();
                Some((f.next()?.to_string(), path.to_string()))
            })?
        })
        .collect()
}

/// Why [`submodules_on_declared_branch`] refused.
#[derive(Debug)]
pub enum PinRefusal {
    /// What a pin's branch carries is unknown: its fetch failed every attempt, or git could not
    /// answer. The network or the forge, not the pin.
    Unread(String),
    /// A pin its declared branch does not carry, or a gitlink with no declared branch.
    Off(String),
}

impl std::fmt::Display for PinRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unread(why) | Self::Off(why) => f.write_str(why),
        }
    }
}

/// Every submodule pin is on its declared branch.
///
/// # Errors
///
/// [`PinRefusal::Off`] for a pin its branch does not carry or a gitlink `.gitmodules` does not
/// declare; [`PinRefusal::Unread`] when git cannot list the tree, every fetch attempt fails, or
/// git cannot decide ancestry.
pub fn submodules_on_declared_branch(
    git: &dyn GitRunner,
    origin: &dyn crate::ledger::Origin,
) -> std::result::Result<(), PinRefusal> {
    let unread = |e: Error| PinRefusal::Unread(e.to_string());
    let listing = git_ok(git, &["ls-tree", "-r", "HEAD"])
        .map_err(unread)?
        .stdout_utf8();
    let links = gitlinks(&listing);
    if links.is_empty() {
        return Ok(());
    }
    let declared = git
        .git(&[
            "config",
            "-f",
            ".gitmodules",
            "--get-regexp",
            r"^submodule\..*\.path$",
        ])
        .map_err(unread)?
        .stdout_utf8();
    let mut off = Vec::new();
    for (pin, path) in &links {
        let Some(name) = declared.lines().find_map(|l| {
            let (key, at) = l.split_once(' ')?;
            (at.trim() == path)
                .then(|| key.strip_prefix("submodule.")?.strip_suffix(".path"))
                .flatten()
        }) else {
            return Err(PinRefusal::Off(format!(
                "the gitlink {path} has no .gitmodules entry, so the branch it must be on is \
                 unknown"
            )));
        };
        let branch = git
            .git(&[
                "config",
                "-f",
                ".gitmodules",
                "--get",
                &format!("submodule.{name}.branch"),
            ])
            .map_err(unread)?;
        let (branch, source) = if branch.success() {
            let branch = branch.stdout_utf8().trim().to_string();
            let source = format!("refs/heads/{branch}");
            (branch, source)
        } else {
            ("HEAD".to_string(), "HEAD".to_string())
        };
        let tip = origin
            .submodule_tip(Path::new(path), &source)
            .map_err(|failure| {
                PinRefusal::Unread(format!(
                    "cannot fetch {path}'s declared branch {branch} from its origin: {failure}, so \
                 whether its pin {pin} is on it is unknown"
                ))
            })?;
        let on = git
            .git(&["-C", path, "merge-base", "--is-ancestor", pin, &tip])
            .map_err(unread)?;
        match on.status {
            0 => {}
            1 => off.push(format!("{path} at {pin} is not on {branch}")),
            n => {
                return Err(PinRefusal::Unread(format!(
                    "git cannot tell whether {path}'s pin {pin} is on {branch} (exit {n}): {}",
                    on.stderr_utf8().trim()
                )));
            }
        }
    }
    if off.is_empty() {
        return Ok(());
    }
    Err(PinRefusal::Off(format!(
        "submodule pin(s) on no commit their declared branch carries — a release ships only \
         what each repository's own branch carries:\n  {}",
        off.join("\n  ")
    )))
}

/// What a claim commit changes on top of the commit it claims.
pub const CLAIM_FILES: [&str; 2] = [crate::changelog::CHANGELOG_FILE, crate::ledger::LEDGER_FILE];

/// The cutter is its commit's own.
///
/// # Errors
/// This binary is not `commit`'s cutter: why, and the remedy.
pub fn own_cutter(
    git: &dyn GitRunner,
    commit: &str,
) -> std::result::Result<String, crate::cutter::NotOwn> {
    own_cutter_stamp_of(git, crate::cutter::running_stamp(), commit)
}

/// Whether the release flow is already running the content-proved frozen cutter for `commit`.
/// Hand-run verbs keep [`own_cutter`]'s dep-info judgment; only the release flow requires this
/// stronger boundary so even an exact tree build freezes both tools once before qualification.
///
/// # Errors
/// The running image is not the frozen cutter proved for exactly `commit`.
pub fn own_frozen_cutter(commit: &str) -> std::result::Result<String, crate::cutter::NotOwn> {
    own_frozen_cutter_stamp_of(crate::cutter::running_stamp(), commit)
}

fn own_frozen_cutter_stamp_of(
    stamp: std::result::Result<&crate::cutter::Stamp, &crate::cutter::NotOwn>,
    commit: &str,
) -> std::result::Result<String, crate::cutter::NotOwn> {
    match stamp {
        Ok(crate::cutter::Stamp::Proved(stamp)) if stamp == commit => Ok(format!(
            "cutter identity: content-proved from {} itself",
            short(commit)
        )),
        Ok(crate::cutter::Stamp::Proved(stamp)) => Err(crate::cutter::NotOwn {
            why: format!("this content-proved aterm-release binary is for {stamp}, not {commit}"),
            fix: format!(
                "run `{}` again from the clean release clone",
                crate::release::RELEASE_COMMAND
            ),
        }),
        Ok(crate::cutter::Stamp::Built(stamp)) => Err(crate::cutter::NotOwn {
            why: format!(
                "this aterm-release binary is the dep-info build of {stamp}, not the frozen \
                 content-proved cutter for {commit}"
            ),
            fix: format!(
                "run `{}` again from the clean release clone; it freezes and execs that \
                 commit's tools",
                crate::release::RELEASE_COMMAND
            ),
        }),
        Err(none) => Err(crate::cutter::NotOwn {
            why: format!(
                "this aterm-release binary is no commit's cutter: {}",
                none.why
            ),
            fix: none.fix.clone(),
        }),
    }
}

/// [`own_cutter`] for a cutter whose verdict is `stamp`.
///
/// # Errors
/// As [`own_cutter`].
fn own_cutter_stamp_of(
    git: &dyn GitRunner,
    stamp: std::result::Result<&crate::cutter::Stamp, &crate::cutter::NotOwn>,
    commit: &str,
) -> std::result::Result<String, crate::cutter::NotOwn> {
    match stamp {
        Ok(crate::cutter::Stamp::Proved(stamp)) if stamp == commit => Ok(format!(
            "cutter identity: content-proved from {} itself",
            short(commit)
        )),
        // The cut claims R in a child commit that adds only CLAIM_FILES, which no cutter
        // compiles in, so R's content proof covers it as it covers R. Without this a resume
        // past the claim could run no cutter at all: the release flow freezes R's.
        Ok(crate::cutter::Stamp::Proved(stamp)) if claims_only(git, stamp, commit) => Ok(format!(
            "cutter identity: content-proved from {}, whose claim commit {} adds only its \
                 ledger line and changelog roll",
            short(stamp),
            short(commit)
        )),
        Ok(crate::cutter::Stamp::Proved(stamp)) => Err(crate::cutter::NotOwn {
            why: format!("this content-proved aterm-release binary is for {stamp}, not {commit}"),
            fix: format!(
                "run `{}` again from the clean release clone",
                crate::release::RELEASE_COMMAND
            ),
        }),
        Ok(crate::cutter::Stamp::Built(stamp)) => {
            own_cutter_with(git, stamp, commit).map_err(|why| crate::cutter::NotOwn {
                why,
                fix: format!(
                    "run it as `{}` from the release's clone (it freezes and executes the release \
                     commit's own cutter), or as `targo --unverified ship <verb>` from a clean \
                     checkout of {commit} with its own target directory (cargo relinks the \
                     cutter from that tree before it runs it)",
                    crate::release::RELEASE_COMMAND
                ),
            })
        }
        Err(none) => Err(crate::cutter::NotOwn {
            why: format!(
                "this aterm-release binary is no commit's cutter: {}",
                none.why
            ),
            fix: none.fix.clone(),
        }),
    }
}

/// The dep-info form used by prechecks and their pure seams.
fn own_cutter_of(
    git: &dyn GitRunner,
    stamp: std::result::Result<&str, &crate::cutter::NotOwn>,
    commit: &str,
) -> std::result::Result<String, crate::cutter::NotOwn> {
    match stamp {
        Ok(stamp) => {
            let built = crate::cutter::Stamp::Built(stamp.to_owned());
            own_cutter_stamp_of(git, Ok(&built), commit)
        }
        Err(not) => own_cutter_stamp_of(git, Err(not), commit),
    }
}

/// [`own_cutter`] for a cutter that is the build of `stamp` — pure over the stamp.
///
/// # Errors
/// As [`own_cutter`].
pub fn own_cutter_with(
    git: &dyn GitRunner,
    stamp: &str,
    commit: &str,
) -> std::result::Result<String, String> {
    if !aterm_codec::hex::is_lower(stamp, 40) {
        return Err(format!(
            "this aterm-release binary's build commit {stamp:?} is not a commit id"
        ));
    }
    if stamp == commit {
        return Ok(format!(
            "cutter identity: built from {} itself",
            short(commit)
        ));
    }
    if claims_only(git, stamp, commit) {
        return Ok(format!(
            "cutter identity: built from {}, whose claim commit {} adds only its ledger \
             line and changelog roll",
            short(stamp),
            short(commit)
        ));
    }
    Err(format!(
        "this aterm-release binary was built from {stamp}, not {commit}"
    ))
}

/// Whether `commit` is `stamp`'s claim commit: its first parent is `stamp` and it changes
/// at least one of [`CLAIM_FILES`] and nothing else.
fn claims_only(git: &dyn GitRunner, stamp: &str, commit: &str) -> bool {
    let parent = git
        .git(&["rev-parse", "--verify", "-q", &format!("{commit}^1")])
        .ok()
        .filter(crate::ledger::RunOut::success)
        .map(|out| out.stdout_utf8().trim().to_string());
    if parent.as_deref() != Some(stamp) {
        return false;
    }
    let Ok(out) = git.git(&["diff", "--name-only", "--no-renames", stamp, commit]) else {
        return false;
    };
    if !out.success() {
        return false;
    }
    let text = out.stdout_utf8();
    let changed: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    !changed.is_empty() && changed.iter().all(|p| CLAIM_FILES.contains(p))
}

/// [`own_cutter`] as a gate, with its remedy.
///
/// # Errors
/// This binary is not `commit`'s cutter, with the remedy.
pub fn own_cutter_gate(git: &dyn GitRunner, commit: &str, allow_stale: bool) -> Result<()> {
    match own_cutter(git, commit) {
        Ok(_) => Ok(()),
        Err(not) if allow_stale => {
            println!(
                "==> cutter identity: {} — allowed because this dry-run publishes nowhere, but \
                 it is running OTHER code than the tree",
                not.why
            );
            Ok(())
        }
        Err(not) => Err(Error::new(format!("{}.\nfix:  {}", not.why, not.fix))),
    }
}

/// [`own_cutter_gate`] for the checkout the caller is about to act from (its `HEAD`).
///
/// # Errors
/// As [`own_cutter_gate`].
pub fn current_own_cutter_gate(git: &dyn GitRunner) -> Result<()> {
    own_cutter_gate(git, &rev_parse(git, "HEAD")?, false)
}

/// A publishing cut requires an unused tag locally and on origin. A signing
/// rehearsal claims no tag and does not need to query either namespace.
fn tag_gate(
    git: &dyn GitRunner,
    origin: Option<&dyn crate::ledger::Origin>,
    version: &str,
    publishes: bool,
) -> Result<()> {
    if !publishes {
        return Ok(());
    }
    let origin = origin.ok_or_else(|| Error::new("a publishing cut needs its native origin"))?;
    let tag = format!("v{version}");
    let local = git.git(&["rev-parse", "-q", "--verify", &format!("refs/tags/{tag}")])?;
    if origin
        .get(&format!("refs/tags/{tag}"))
        .map_err(Error::new)?
        .is_some()
    {
        return Err(Error::new(format!(
            "tag {tag} is on origin: v{version} was cut. Start the next product release with \
             `{} --bump`; it reserves the version through the source claim",
            crate::release::RELEASE_COMMAND
        )));
    }
    if local.success() {
        return Err(Error::new(format!(
            "tag {tag} exists locally but not on origin: `git tag -d {tag}`, then cut again"
        )));
    }
    Ok(())
}

/// The changelog gate (changelog.rs) for a commit, read from its objects.
///
/// # Errors
/// The commit holds no changelog, or the cut's changelog refusal.
pub fn changelog_gate_at(
    git: &dyn GitRunner,
    commit: &str,
    version: &str,
    catalog: &str,
) -> Result<(String, changelog::GateSummary)> {
    changelog_gate_at_with(git, commit, version, catalog, |at, text, section, keys| {
        changelog::gate_history(git, at, text, section, keys)
    })
}

fn changelog_gate_at_with(
    git: &dyn GitRunner,
    commit: &str,
    version: &str,
    catalog: &str,
    mut audit: impl FnMut(&str, &str, &str, &std::collections::BTreeSet<String>) -> Result<()>,
) -> Result<(String, changelog::GateSummary)> {
    let text = GateTree::Commit { git, commit }.text(changelog::CHANGELOG_FILE)?;
    let section = if changelog::has_section(&text, version) {
        version
    } else {
        "Unreleased"
    };
    let found = changelog_section_gate(&text, section)?;
    let publication = changelog::gate_published(git, commit, version, &text, section, catalog)?;
    let mut pending: std::collections::BTreeSet<_> =
        changelog::note_keys(&text, section).into_iter().collect();
    if let Some(publication) = publication {
        let published = pending
            .intersection(&publication.entries)
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        pending.retain(|key| !publication.entries.contains(key));
        if !published.is_empty() {
            audit(
                &publication.source,
                &publication.text,
                &publication.section,
                &published,
            )?;
        }
    }
    if !pending.is_empty() {
        audit(commit, &text, section, &pending)?;
    }
    Ok((section.to_string(), found))
}

fn changelog_section_gate(text: &str, section: &str) -> Result<changelog::GateSummary> {
    if section == "Unreleased" {
        changelog::gate_unreleased(text)
    } else {
        changelog::gate_section(text, section)
    }
}

/// Where each shipped release's handoff producer bytes are checked in, one directory per release (`v<version>/`) and one per desk inside it.
pub const HANDOFF_FIXTURE_ROOT: &str = "crates/aterm-gui/tests/fixtures/handoff";

/// How to add a release's fixtures — the one place the steps are written down.
pub const HANDOFF_FIXTURE_README: &str = "crates/aterm-gui/tests/fixtures/handoff/README.md";

/// The guard that runs every fixture through the current consumer, and whose `PINNED_DESKS` table is what makes a lost fixture directory a red test.
pub const HANDOFF_FIXTURE_GUARD: &str = "crates/aterm-gui/src/seamless_fixture_tests.rs";

/// What [`handoff_fixture_gate_at`] found for the release a cut succeeds.
#[derive(Debug, PartialEq, Eq)]
pub struct HandoffFixtures {
    /// The release, as the ledger records it (`0.92.0`).
    pub release: String,
    /// Its desk directories that hold a `parent.toml`, sorted.
    pub desks: Vec<String>,
    /// Its desks named in the guard's `PINNED_DESKS`, in table order.
    pub pinned: Vec<String>,
}

/// Refuse a release whose predecessor's handoff fixtures are not checked in and pinned, judged on the commit.
pub fn handoff_fixture_gate_at(
    git: &dyn GitRunner,
    origin: &dyn crate::ledger::Origin,
    commit: &str,
    version: &str,
) -> Result<Option<HandoffFixtures>> {
    handoff_fixture_gate_in(origin, &GateTree::Commit { git, commit }, version)
}

/// Where a gate reads the tree it judges: a commit's objects.
pub(crate) enum GateTree<'a> {
    #[cfg(test)]
    Dir(&'a Path),
    Commit {
        git: &'a dyn GitRunner,
        commit: &'a str,
    },
}

impl GateTree<'_> {
    /// The text of the tracked file `path`.
    pub(crate) fn text(&self, path: &str) -> Result<String> {
        match self {
            #[cfg(test)]
            GateTree::Dir(repo) => {
                let full = repo.join(path);
                fs::read_to_string(&full)
                    .map_err(|e| Error::new(format!("cannot read {}: {e}", full.display())))
            }
            GateTree::Commit { git, commit } => {
                git_ok(*git, &["cat-file", "blob", &format!("{commit}:{path}")])
                    .map(|out| out.stdout_utf8())
                    .map_err(|e| Error::new(format!("cannot read {path} at {commit}: {e}")))
            }
        }
    }

    /// The directories directly under `dir` that hold a `parent.toml`, sorted.
    pub(crate) fn desks(&self, dir: &str) -> Result<Option<Vec<String>>> {
        match self {
            #[cfg(test)]
            GateTree::Dir(repo) => {
                let entries = match fs::read_dir(repo.join(dir)) {
                    Ok(entries) => entries,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(e) => return Err(Error::new(format!("cannot read {dir}/: {e}"))),
                };
                let mut desks = Vec::new();
                for entry in entries {
                    let entry =
                        entry.map_err(|e| Error::new(format!("cannot read {dir}/: {e}")))?;
                    if entry.path().join("parent.toml").is_file() {
                        desks.push(entry.file_name().to_string_lossy().into_owned());
                    }
                }
                desks.sort_unstable();
                Ok(Some(desks))
            }
            GateTree::Commit { git, commit } => {
                let listed = git_ok(
                    *git,
                    &["ls-tree", "-r", "-z", "--name-only", *commit, "--", dir],
                )?
                .stdout_utf8();
                if listed.split('\0').all(str::is_empty) {
                    return Ok(None);
                }
                let prefix = format!("{dir}/");
                let mut desks: Vec<String> = listed
                    .split('\0')
                    .filter_map(|path| path.strip_prefix(prefix.as_str()))
                    .filter_map(|rest| rest.strip_suffix("/parent.toml"))
                    .filter(|desk| !desk.is_empty() && !desk.contains('/'))
                    .map(str::to_string)
                    .collect();
                desks.sort_unstable();
                desks.dedup();
                Ok(Some(desks))
            }
        }
    }
}

fn handoff_fixture_gate_in(
    origin: &dyn crate::ledger::Origin,
    tree: &GateTree<'_>,
    version: &str,
) -> Result<Option<HandoffFixtures>> {
    let predecessor = predecessor_in(origin, tree, version)
        .map_err(|e| Error::new(format!("handoff-fixture gate: {e}")))?;
    match predecessor {
        Some(release) => handoff_fixtures_of(tree, &release).map(Some),
        None => Ok(None),
    }
}

/// The release the cut succeeds ([`predecessor_release`]), from `tree`'s ledger and origin's
/// tags.
///
/// # Errors
/// The ledger cannot be read or parsed, origin's tags cannot be listed, or none of the earlier
/// versions shipped. The message names no gate: each caller says which of its gates asked
/// (`handoff-fixture gate:`, `handoff-policy gate:`).
fn predecessor_in(
    origin: &dyn crate::ledger::Origin,
    tree: &GateTree<'_>,
    version: &str,
) -> Result<Option<String>> {
    let ledger_text = tree.text(crate::ledger::LEDGER_FILE)?;
    let listing = origin.tag_catalog().map_err(|e| {
        Error::new(format!(
            "cannot list origin's tags, which is how the gate tells the releases that shipped \
             from claims that were abandoned: {e}"
        ))
    })?;
    let tags = tag_commits(&listing);
    let shipped = |v: &str| tags.contains_key(&format!("v{v}"));
    predecessor_release(&ledger_text, version, &shipped)
}

pub(crate) fn tag_commits(listing: &str) -> std::collections::BTreeMap<String, String> {
    let mut tags = std::collections::BTreeMap::new();
    for line in listing.lines() {
        let mut fields = line.split_whitespace();
        let (Some(oid), Some(tag)) = (
            fields.next(),
            fields
                .next()
                .and_then(|refname| refname.strip_prefix("refs/tags/")),
        ) else {
            continue;
        };
        match tag.strip_suffix("^{}") {
            Some(annotated) => {
                tags.insert(annotated.to_string(), oid.to_string());
            }
            None => {
                tags.entry(tag.to_string())
                    .or_insert_with(|| oid.to_string());
            }
        }
    }
    tags
}

/// The newest release the ledger records before the one being cut that `shipped` says has a tag.
///
/// # Errors
/// The ledger cannot be parsed, or names earlier versions none of which shipped. The message
/// names no gate: each caller says which of its gates asked.
pub fn predecessor_release(
    ledger_text: &str,
    version: &str,
    shipped: &dyn Fn(&str) -> bool,
) -> Result<Option<String>> {
    recorded_release(ledger_text, &|candidate| candidate != version, shipped)
}

/// The newest recorded shipped release at or below a full three-component version.
/// Unlike constructing a successor as a fence, this also inspects the greatest valid counter.
pub(crate) fn release_through(
    ledger_text: &str,
    version: &str,
    shipped: &dyn Fn(&str) -> bool,
) -> Result<Option<String>> {
    first_shipped(release_candidates_through(ledger_text, version)?, shipped)
}

/// Ordered, deduplicated full release identities eligible for an inclusive
/// public-tag query. Historical ledger labels without three components remain
/// untouched in the ledger and are not numeric release candidates.
pub(crate) fn release_candidates_through(ledger_text: &str, version: &str) -> Result<Vec<String>> {
    let ceiling = version_triple(version)
        .ok_or_else(|| Error::new(format!("cannot order product release {version}")))?;
    recorded_candidates(ledger_text, &|candidate| {
        version_triple(candidate).is_some_and(|key| key <= ceiling)
    })
}

fn recorded_release(
    ledger_text: &str,
    eligible: &dyn Fn(&str) -> bool,
    shipped: &dyn Fn(&str) -> bool,
) -> Result<Option<String>> {
    first_shipped(recorded_candidates(ledger_text, eligible)?, shipped)
}

fn recorded_candidates(ledger_text: &str, eligible: &dyn Fn(&str) -> bool) -> Result<Vec<String>> {
    let mut candidates = Vec::new();
    for record in crate::ledger::parse(ledger_text)?.into_iter().rev() {
        let candidate = record.version;
        if !eligible(&candidate) || candidates.contains(&candidate) {
            continue;
        }
        candidates.push(candidate);
    }
    Ok(candidates)
}

fn first_shipped(
    candidates: Vec<String>,
    shipped: &dyn Fn(&str) -> bool,
) -> Result<Option<String>> {
    if let Some(candidate) = candidates.iter().find(|candidate| shipped(candidate)) {
        return Ok(Some(candidate.clone()));
    }
    match candidates.first() {
        None => Ok(None),
        Some(newest) => Err(Error::new(format!(
            "none of the {} earlier versions in {} (newest {newest}) has its v<version> tag \
             on origin, so no shipped release precedes this cut — is origin the repository \
             this ledger records?",
            candidates.len(),
            crate::ledger::LEDGER_FILE,
        ))),
    }
}

/// Judge one release's fixtures.
pub(crate) fn handoff_fixtures_of(tree: &GateTree<'_>, release: &str) -> Result<HandoffFixtures> {
    let rel_dir = format!("{HANDOFF_FIXTURE_ROOT}/v{release}");
    let Some(desks) = tree.desks(&rel_dir)? else {
        return Err(handoff_refusal(
            release,
            &format!("{rel_dir}/ does not exist"),
        ));
    };
    if desks.is_empty() {
        return Err(handoff_refusal(
            release,
            &format!("{rel_dir}/ holds no desk with a parent.toml"),
        ));
    }
    let guard = tree.text(HANDOFF_FIXTURE_GUARD)?;
    let tag = format!("v{release}");
    let pinned: Vec<String> = pinned_desk_slices(&guard)?
        .into_iter()
        .filter(|(row_release, _)| *row_release == tag)
        .map(|(_, desk)| desk.to_string())
        .collect();
    if pinned.is_empty() {
        return Err(handoff_refusal(
            release,
            &format!(
                "{HANDOFF_FIXTURE_GUARD} has no PINNED_DESKS row for {tag} (desks on disk: {})",
                desks.join(", ")
            ),
        ));
    }
    let mut missing = Vec::new();
    for required in required_desks(&guard)? {
        if release_at_least(release, &required.from)?
            && (!pinned.contains(&required.desk) || !desks.contains(&required.desk))
        {
            missing.push(format!(
                "{} (required from {} on)",
                required.desk, required.from
            ));
        }
    }
    if !missing.is_empty() {
        return Err(handoff_refusal(
            release,
            &format!(
                "{tag} lacks the required desk(s) {} (REQUIRED_DESKS in \
                 {HANDOFF_FIXTURE_GUARD}: each on disk with a parent.toml and pinned in \
                 PINNED_DESKS from its floor on; on disk: {}; pinned: {})",
                missing.join(", "),
                desks.join(", "),
                pinned.join(", ")
            ),
        ));
    }
    Ok(HandoffFixtures {
        release: release.to_string(),
        desks,
        pinned,
    })
}

/// The `(release, desk)` rows of the guard's `PINNED_DESKS` table, read from its source.
pub(crate) fn pinned_desks(guard: &str) -> Result<Vec<(String, String)>> {
    Ok(pinned_desk_slices(guard)?
        .into_iter()
        .map(|(rel, desk)| (rel.to_string(), desk.to_string()))
        .collect())
}

fn pinned_desk_slices(guard: &str) -> Result<Vec<(&str, &str)>> {
    let unreadable = |why: &str| {
        Error::new(format!(
            "handoff-fixture gate: cannot read the PINNED_DESKS table in \
             {HANDOFF_FIXTURE_GUARD} ({why}); the gate reads the pinned rows from its source"
        ))
    };
    let table = guard
        .find("const PINNED_DESKS")
        .map(|at| &guard[at..])
        .ok_or_else(|| unreadable("no `const PINNED_DESKS`"))?;
    let body = table
        .find("= &[")
        .map(|at| &table[at + "= &[".len()..])
        .and_then(|rest| rest.find("];").map(|end| &rest[..end]))
        .ok_or_else(|| unreadable("no `= &[ … ];` after it"))?;
    let literals: Vec<&str> = body
        .lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .flat_map(|line| line.split('"').skip(1).step_by(2))
        .collect();
    if !literals.len().is_multiple_of(2) {
        return Err(unreadable("an odd number of string literals"));
    }
    Ok(literals.chunks(2).map(|row| (row[0], row[1])).collect())
}

/// One row of the guard's required desk set: `desk` is required of every release from `from` on.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RequiredDesk {
    /// The first release held to it, as the ledger spells a version.
    pub(crate) from: String,
    pub(crate) desk: String,
}

/// The guard's `REQUIRED_DESKS` table.
pub(crate) fn required_desks(guard: &str) -> Result<Vec<RequiredDesk>> {
    let unreadable = |why: &str| {
        Error::new(format!(
            "handoff-fixture gate: cannot read the required desk set in \
             {HANDOFF_FIXTURE_GUARD} ({why}); the gate reads REQUIRED_DESKS, one \
             (\"vX.Y.Z\", \"<desk>\") row per desk, from its source"
        ))
    };
    let table = guard
        .find("const REQUIRED_DESKS:")
        .map(|at| &guard[at..])
        .ok_or_else(|| unreadable("no `const REQUIRED_DESKS:`"))?;
    let body = table
        .find("= &[")
        .map(|at| &table[at + "= &[".len()..])
        .and_then(|rest| rest.find("];").map(|end| &rest[..end]))
        .ok_or_else(|| unreadable("no `= &[ … ];` after REQUIRED_DESKS"))?;
    let literals: Vec<&str> = body
        .lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .flat_map(|line| line.split('"').skip(1).step_by(2))
        .collect();
    if literals.is_empty() {
        return Err(unreadable("REQUIRED_DESKS names no desk"));
    }
    if !literals.len().is_multiple_of(2) {
        return Err(unreadable("an odd number of string literals"));
    }
    literals
        .chunks(2)
        .map(|row| {
            let from = row[0]
                .strip_prefix('v')
                .filter(|version| version_triple(version).is_some())
                .ok_or_else(|| {
                    unreadable(&format!(
                        "the floor of `{}` is not a \"vX.Y.Z\" literal",
                        row[1]
                    ))
                })?;
            Ok(RequiredDesk {
                from: from.to_string(),
                desk: row[1].to_string(),
            })
        })
        .collect()
}

/// The canonical product-release ordering key. Historical two-component ledger
/// entries are not candidates for a current three-component release window.
pub(crate) fn version_triple(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.').map(str::parse::<u64>);
    let triple = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    parts.next().is_none().then_some(triple)
}

/// Whether `release` is `floor` or later.
pub(crate) fn release_at_least(release: &str, floor: &str) -> Result<bool> {
    match (version_triple(release), version_triple(floor)) {
        (Some(release), Some(floor)) => Ok(release >= floor),
        _ => Err(Error::new(format!(
            "handoff-fixture gate: cannot order release {release} against the required \
             desk set's floor {floor}"
        ))),
    }
}

/// Compile only the missing fixture target, recording Cargo artifacts for direct execution.
pub(crate) fn aterm_gui_lib_compile(targo: &Path, tree: &Path, target: &Path) -> Command {
    let mut command = command(targo);
    command
        .args([
            "--unverified",
            "test",
            "-p",
            "aterm-gui",
            "--lib",
            "--target-dir",
        ])
        .arg(target)
        .args(["--no-run", "--message-format=json-render-diagnostics"])
        .current_dir(tree)
        .env_remove("RUSTFLAGS")
        .env_remove("RUSTDOCFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTC")
        .env_remove("RUSTC_BOOTSTRAP")
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET")
        .stdin(aterm_uds::spawnfd::Stdio::null());
    command
}

/// How much longer `dir` stays where it is: a program was started from it
/// ([`aterm_uds::deadline::stamp_exec`]) and never showed it ran, so macOS may still be
/// assessing its first exec, until [`aterm_uds::deadline::FIRST_EXEC_HANG`] of awake time
/// has passed or the Mac restarted. A directory with no stamp holds nothing.
pub(crate) fn first_exec_hold(dir: &Path) -> Option<std::time::Duration> {
    fs::symlink_metadata(dir.join(aterm_uds::deadline::EXEC_STAMP)).ok()?;
    aterm_uds::deadline::copy_wait_left(dir, std::time::SystemTime::now())
}

/// Remove `path` (a directory, a file or a link), and nothing when it is absent.
pub(crate) fn clear_path(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// What libtest's output says.
pub(crate) fn libtest_tally(stdout: &str) -> (usize, usize, Vec<&str>) {
    let mut passed = 0_usize;
    let mut failed = 0_usize;
    for line in stdout.lines() {
        let Some(result) = line.trim().strip_prefix("test result: ") else {
            continue;
        };
        for part in result.split(';') {
            let mut words = part.split_whitespace().rev();
            let (Some(kind), Some(count)) = (words.next(), words.next()) else {
                continue;
            };
            if !count.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let Ok(count) = count.parse::<usize>() else {
                continue;
            };
            match kind {
                "passed" => passed += count,
                "failed" => failed += count,
                _ => {}
            }
        }
    }
    let failing = stdout
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("test ")
                .and_then(|rest| rest.strip_suffix(" ... FAILED"))
        })
        .collect();
    (passed, failed, failing)
}

/// The first `error` lines of a build that stopped before its tests ran.
pub(crate) fn build_error_lines(stderr: &str) -> String {
    let tail: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("error"))
        .take(5)
        .collect();
    if tail.is_empty() {
        "(no error line)".to_string()
    } else {
        tail.join("\n")
    }
}

fn handoff_refusal(release: &str, missing: &str) -> Error {
    Error::new(format!(
        "the handoff fixtures of v{release}, the release this cut succeeds, are not checked in: \
         {missing}. Nothing was claimed.\n\
         fix:  v{release}'s own cut lands them after it publishes (run `targo \
         --unverified ship release` again in the clone that cut it); a release an older \
         cutter cut runs its generator at tag v{release} by hand ({HANDOFF_FIXTURE_README}, \
         \"By hand\")"
    ))
}

/// The release token must be present and GitHub must take it (`GET /user` names its login),
/// before the build; an offline run checks nothing.
///
/// # Errors
/// No token in its file, a token GitHub refuses, or no answer from GitHub.
pub fn github_identity_gate(offline: bool) -> Result<Option<String>> {
    if offline {
        return Ok(None);
    }
    let token_path = crate::publish::channel_token_path().map_or_else(
        || "~/.secrets/gh_access_token_alabsystems".to_string(),
        |path| path.display().to_string(),
    );
    if !crate::github::has_token() {
        return Err(Error::new(format!(
            "no release token in {token_path}: the cut publishes as the release org's token, \
             so put it there (one line, nothing else); nothing was claimed"
        )));
    }
    let user = crate::github::get("user").map_err(|e| {
        Error::new(format!(
            "GitHub did not take the release token in {token_path}: {e}; nothing was claimed"
        ))
    })?;
    Ok(user["login"].as_str().map(str::to_string))
}

const OFF_SWITCH_NOTE: &str = "compiled WITHOUT Trust verification";

const PROBE_SRC: &str = "\
pub fn divide(numerator: u32, denominator: u32) -> u32 {
    numerator / denominator
}

fn main() {
    let _ = divide(6, 3);
}
";

fn verification_off_verdict(stderr: &str) -> Result<()> {
    if !stderr.contains(OFF_SWITCH_NOTE) {
        return Err(Error::new(format!(
            "the probe compiled through `targo --unverified` and trustc did NOT report \
             \"{OFF_SWITCH_NOTE}\": the lane did not switch verification off, so the real \
             build would verify every unit and fail. trustc said: {}\n\
             fix:  check that `targo --unverified --version` is accepted (upstream cargo \
             rejects it) and that `trustc -Z help | grep trust-verify` lists the switch",
            stderr
                .lines()
                .find(|line| line.contains("Trust verification"))
                .unwrap_or("(no Trust verification line)")
                .trim()
        )));
    }
    Ok(())
}

/// Probe the trustc the native slice uses (the Trust stage2 compiler `targo` drives).
pub fn trustc_probe(repo: &Path) -> Result<PathBuf> {
    let trustc = trust_stage2_bin()?.join("trustc");
    let probe = command(&trustc).arg("--version").output();
    match probe {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            return Err(Error::new(format!(
                "trustc at {} exists but failed --version: {}",
                trustc.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Err(e) => {
            return Err(Error::new(format!(
                "trustc not runnable at {} ({e}) — the repo compiles with Trust always. \
                 Reinstall the toolchain (`aterm pkg install trust`, then `aterm pkg doctor`), \
                 or from source rebuild the stage2 ({TRUST_FROM_SOURCE}), \
                 or point TRUST_STAGE2_BIN at a stage2 bin dir that carries trustc",
                trustc.display()
            )));
        }
    }
    let targo = resolve_targo()?;
    let probe_dir = aterm_tempfile::Builder::new()
        .prefix("aterm-trust-probe-")
        .suffix(".noindex")
        .tempdir()
        .map_err(|e| Error::new(format!("probe scratch: {e}")))?;
    let dir = probe_dir.path();
    let setup = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(dir.join("src"))?;
        std::fs::create_dir_all(dir.join(".cargo"))?;
        crate::durable::write_fresh(
            &dir.join("Cargo.toml"),
            "[package]\nname = \"aterm-trust-probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
              [workspace]\n",
        )?;
        crate::durable::write_fresh(&dir.join("src/main.rs"), PROBE_SRC)?;
        crate::durable::copy_regular_fresh(
            repo.join(".cargo/config.toml"),
            dir.join(".cargo/config.toml"),
        )?;
        Ok(())
    })();
    if let Err(e) = setup {
        return Err(Error::new(format!(
            "probe package in {}: {e}",
            dir.display()
        )));
    }
    let out = command(&targo)
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", dir.join("target"))
        .env("CARGO_HOME", dir.join("cargo-home"))
        .env_remove("RUSTC")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_BOOTSTRAP")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_BUILD_RUSTFLAGS")
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("RUSTUP_TOOLCHAIN")
        .args(["--unverified", "check", "--offline"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            verification_off_verdict(&String::from_utf8_lossy(&o.stderr)).map(|()| trustc.clone())
        }
        Ok(o) => Err(Error::new(format!(
            "trustc at {} runs but `{} --unverified check` cannot COMPILE the probe under \
             the repo's config (stage2 library missing/stale — reinstall \
             with `aterm pkg install trust`, or from source rebuild {TRUST_FROM_SOURCE} — or \
             a flag spelling does not match this trustc; `trustc -Z help | grep trust-verify` \
             decides): {}",
            trustc.display(),
            targo.display(),
            String::from_utf8_lossy(&o.stderr)
                .lines()
                .find(|l| l.starts_with("error"))
                .unwrap_or("(no error line)")
        ))),
        Err(e) => Err(Error::new(format!("probe compile spawn: {e}"))),
    }
}

/// Probe stable's installed `x86_64-apple-darwin` target (STOCK EXCEPTION: Trust lacks an `x86_64-apple-darwin` std).
pub fn x86_target_probe() -> Result<()> {
    if stable_targets_missing(&["x86_64-apple-darwin"])?.is_empty() {
        return Ok(());
    }
    Err(Error::new(
        "x86_64-apple-darwin target missing from the stable toolchain — a universal \
         build is impossible"
            .to_string(),
    ))
}

/// Which of `targets` upstream stable has no std installed for.
///
/// # Errors
/// rustup cannot be run, or does not answer.
pub fn stable_targets_missing(targets: &[&str]) -> Result<Vec<String>> {
    let rustup = aterm_uds::host::tool("rustup").locate().map_err(|absent| {
        Error::new(format!(
            "{}; upstream stable carries the std of the targets Trust has none for.\n\
             fix:  {}",
            absent.why, absent.fix
        ))
    })?;
    let out = command(&rustup)
        .env("RUSTUP_TOOLCHAIN", "stable")
        .args(["target", "list", "--installed"])
        .output()
        .map_err(|e| {
            Error::new(format!(
                "failed to run {} ({e}); upstream stable carries the std of the targets \
                 Trust has none for.\n\
                 fix:  {}",
                rustup.display(),
                aterm_uds::host::tool("rustup").remedy()
            ))
        })?;
    if !out.status.success() {
        return Err(Error::new(format!(
            "`rustup target list --installed` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let installed = String::from_utf8_lossy(&out.stdout);
    Ok(targets
        .iter()
        .filter(|t| !installed.lines().any(|l| l.trim() == **t))
        .map(|t| (*t).to_string())
        .collect())
}

/// The proof, before the release's one build (`artifact::build`), that this builder can build the x86_64 slice (`target`, the stable `x86_64-apple-darwin` std) and run it.
pub(crate) fn universal_gate(
    target: impl FnOnce() -> Result<()>,
    rosetta: impl FnOnce() -> std::result::Result<(), String>,
) -> Result<()> {
    target().map_err(|e| {
        Error::new(format!(
            "{e}\nfix:  {}",
            X86_SLICE_REMEDIES.replace('\n', "\n      ")
        ))
    })?;
    rosetta().map_err(Error::new)?;
    Ok(())
}

/// The refusal a release build gets on a builder that cannot RUN its x86_64 slice.
pub(crate) const ROSETTA_REFUSAL: &str = "the universal build RUNS its x86_64 slice under Rosetta before it \
     ships (an unexecuted slice is how every Intel-only fault reached a release), and Rosetta \
     does not run here — softwareupdate --install-rosetta --agree-to-license";

/// Whether Rosetta runs a trivial x86_64 program here (`/usr/bin/arch -x86_64 /usr/bin/true`).
pub(crate) fn rosetta_runs(
    run: &mut dyn FnMut(&mut Command) -> std::io::Result<std::process::Output>,
) -> std::result::Result<(), String> {
    let mut rosetta = command("/usr/bin/arch");
    rosetta.args(["-x86_64", "/usr/bin/true"]);
    if run(&mut rosetta).is_ok_and(|out| out.status.success()) {
        Ok(())
    } else {
        Err(ROSETTA_REFUSAL.to_string())
    }
}

/// Free-disk gate via `df -Pk` (POSIX output format; std has no statfs).
pub fn disk_gate(repo: &Path) -> Result<u64> {
    let out = command(aterm_uds::os_path!("df"))
        .arg("-Pk")
        .arg(repo)
        .output()
        .map_err(|e| Error::new(format!("failed to run df: {e}")))?;
    if !out.status.success() {
        return Err(Error::new(format!(
            "df -Pk failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let avail_kib: u64 = text
        .lines()
        .nth(1)
        .and_then(|l| l.split_whitespace().nth(3))
        .filter(|f| f.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|f| f.parse().ok())
        .ok_or_else(|| Error::new(format!("could not parse df -Pk output:\n{text}")))?;
    let free_gib = avail_kib / (1024 * 1024);
    if free_gib < MIN_FREE_DISK_GIB {
        return Err(Error::new(format!(
            "only {free_gib} GiB free — a release needs at least {MIN_FREE_DISK_GIB} GiB \
             (its build's two release target trees, the cut's dist/ staging)"
        )));
    }
    Ok(free_gib)
}

#[cfg(test)]
#[path = "gates/fixture_origin.rs"]
pub(crate) mod fixture_origin;

#[cfg(test)]
#[path = "gates_universal_gate_tests.rs"]
mod universal_gate_tests;

#[cfg(test)]
#[path = "gates_cut_lease_tests.rs"]
mod cut_lease_tests;

#[cfg(test)]
#[path = "gates_submodule_tests.rs"]
mod submodule_tests;

#[cfg(test)]
#[path = "gates_cutter_identity_tests.rs"]
mod cutter_identity_tests;

#[cfg(test)]
#[path = "gates_toolchain_pin_tests.rs"]
mod toolchain_pin_tests;

#[cfg(test)]
#[path = "gates_native_lane_probe_tests.rs"]
mod native_lane_probe_tests;

#[cfg(test)]
#[path = "gates_tag_gate_tests.rs"]
mod tag_gate_tests;

#[cfg(test)]
#[path = "gates_handoff_fixture_gate_tests.rs"]
mod handoff_fixture_gate_tests;

#[cfg(test)]
#[path = "gates_handoff_policy_gate_tests.rs"]
mod handoff_policy_gate_tests;

#[cfg(test)]
#[path = "gates_staged_bundle_liveness_tests.rs"]
mod staged_bundle_liveness_tests;

#[cfg(test)]
#[path = "gates_published_commit_tests.rs"]
mod published_commit_tests;

#[cfg(test)]
#[path = "gates_ledger_reader_tests.rs"]
mod ledger_reader_tests;
