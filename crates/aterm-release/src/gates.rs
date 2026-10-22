// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pre-claim gates, each failing closed with its remedy before anything is committed or
//! pushed, so a failed gate costs seconds rather than a ledger number: the cut's moment
//! gates ([`run_all`]) and the commit's own gates that `ship release` judges at step 4 and
//! `ship check` answers (changelog, the predecessor's handoff fixtures and policy, locked
//! metadata, the cross-version handoff guard, the L0 censuses).
//!
//! Git-backed gates go through the [`GitRunner`] seam the claim uses; host-tool gates (`gh`,
//! `rustup`, `df`, the Trust rustc) shell out, since they interrogate this machine.

use aterm_uds::spawnfd::{Command, command};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::changelog;
use crate::channel;
use crate::ledger::{BYTE_EXACT, Error, GitRunner, Result, git_ok, rev_parse};
use crate::release::Refusal;

/// Free-disk floor for the release's build and its cut: the build keeps two full
/// `--release` target trees (Trust arm64 and rustup x86_64, with debug info for the dSYM)
/// and its bundle in the release cache, and the cut stages a copy, the DMG and the zip in
/// `dist/`. Conservative, so it fails before a build dies on ENOSPC.
pub const MIN_FREE_DISK_GIB: u64 = 10;

/// The Trust toolchain's stage2 tool dir (`targo`, `trustc`, `trustdoc`, …), found by
/// atpkg's one resolution order ([`atpkg::toolchain::Toolchain::discover`]):
/// `$TRUST_STAGE2_BIN` (never fallen back from) → the rustup `trust` toolchain → the atpkg
/// store's `store/trust/current/bin` → `PATH`, a rustup entry older than the store's build
/// ranked below the store. The directory carries `targo` and the pinned `trustc`, and is
/// the physical path: the protected Trust drivers refuse a symlinked one.
pub fn trust_stage2_bin() -> Result<PathBuf> {
    // Pinned once per process: the candidate walk ends at the store's `store/trust/current`,
    // a mutable indirection (`aterm pkg install trust` and `promote-toolchain.sh` repoint
    // it), and a cut calls this from several places. Resolving it more than once could
    // split a run across two compilers with nothing going red. Only success is pinned, so
    // an install made on this error's advice is seen.
    let pinned = pin_first_success(&PINNED, resolve_trust_stage2_bin)?;
    if LEASE_ARMED.load(std::sync::atomic::Ordering::Relaxed) {
        hold_toolchain_lease(&pinned);
    }
    Ok(pinned)
}

/// The one toolchain this process pinned ([`trust_stage2_bin`]).
static PINNED: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Pin `dir` as this process's Trust toolchain before anything resolves one: `ship
/// release` holds one prover for its whole life (on a resume, the one its journal names,
/// which may no longer be the store's current build), so every in-process judge that asks
/// [`trust_stage2_bin`] judges that prover.
///
/// # Errors
/// This process already pinned another toolchain.
pub fn pin_trust_stage2_bin(dir: &Path) -> Result<()> {
    pin_into(&PINNED, dir)
}

/// [`pin_trust_stage2_bin`] over `cell` (a test's own cell, the process's in production).
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

/// Whether this process is a cut ([`arm_toolchain_lease`]): only a cut leases the toolchain
/// it pins. Tests resolve the machine's real toolchain too and must never write into the
/// machine's package store.
static LEASE_ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Arm [`hold_toolchain_lease`]: the next [`trust_stage2_bin`] leases the toolchain it
/// answers with. Called once, at the top of a cut (`publish::run_cut`).
pub fn arm_toolchain_lease() {
    LEASE_ARMED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// [`hold_toolchain_lease`] on `dir`, for `ship release`: its one prover, held from the
/// first step to the last (the cut it runs takes its own).
pub fn lease_toolchain(dir: &Path) {
    hold_toolchain_lease(dir);
}

/// The cut's lease on the toolchain it pinned ([`atpkg::lease`]), taken at the first pin
/// after [`arm_toolchain_lease`] and held for the cut's life. The pin keeps a concurrent
/// seal out of a running cut; the lease keeps the package manager from taking the build
/// away: atpkg's gc keeps a leased build, the seam never re-lays a leased rustup view, and
/// an unattended trust update waits (four hours at most). Between two steps no process
/// runs from the directory, so only the lease says it is in use.
///
/// A toolchain outside the atpkg prefix takes none, and a lease that cannot be taken is
/// reported and never stops the cut. Held in a static and never dropped: the kernel
/// releases the lock when the cutter exits, and atpkg reaps a gone holder's file.
fn hold_toolchain_lease(dir: &Path) {
    static LEASE: std::sync::OnceLock<Option<atpkg::lease::Lease>> = std::sync::OnceLock::new();
    LEASE.get_or_init(|| {
        let home = aterm_types::dirs::home_dir();
        let configured = atpkg::config::load().prefix_path(home.as_deref());
        let layout = atpkg::store::resolve(configured.as_deref())?;
        take_cut_lease(&layout.prefix, dir)
    });
}

/// [`hold_toolchain_lease`]'s lease under `prefix`, said on stderr either way it goes.
fn take_cut_lease(prefix: &Path, dir: &Path) -> Option<atpkg::lease::Lease> {
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
            Some(lease)
        }
        Ok(None) => None,
        Err(e) => {
            eprintln!(
                "release: warn \u{2014} toolchain not held ({e}); an update during this cut can \
                 replace {}",
                dir.display()
            );
            None
        }
    }
}

/// Return the cell's value, resolving it exactly once and only on success. Split out so a
/// test can pin the property: a second call with a different resolver still answers the
/// first path, so a mid-run seal cannot change what a running release compiles with.
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
    let found = atpkg::toolchain::Toolchain::discover(
        explicit.as_deref(),
        &home,
        &path,
        Some(atpkg::seam::DEFAULT_SEAM),
    );
    if found.have_targo() && found.stage2_dir.join("trustc").is_file() {
        return Ok(found.stage2_dir);
    }
    // The fault and where it looked; the remedies ([`TRUST_TOOLCHAIN_REMEDIES`]) go under
    // the caller's own `fix:` / `or:`.
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

/// The way past a missing x86_64 slice, for a caller to append verbatim under its own
/// `fix:` marker.
pub const X86_SLICE_REMEDIES: &str = "rustup +stable target add x86_64-apple-darwin";

/// How to build Trust from source, for every remedy that offers it: from the pinned Trust
/// seed, then sealed. A macro so [`TRUST_TOOLCHAIN_REMEDIES`] can `concat!` it; everything
/// else names [`TRUST_FROM_SOURCE`].
///
/// Never a bare `python3 x.py build --stage 2`: that runs whatever stage0 the checkout's
/// gitignored `bootstrap.toml` names, which can be stock Rust under Trust names. The
/// recreator's `--require-seed --fresh-seed` takes the checksum-pinned seed trust's
/// `src/stage0` names and refuses to wrap a stock compiler; `--no-register` leaves rustup
/// alone until `promote-toolchain.sh` points it at the seal.
macro_rules! trust_from_source {
    () => {
        "in a Trust checkout, python3 scripts/recreate_bootstrap.py --stage 2 --require-seed \
         --fresh-seed --no-register (stage0 = the pinned Trust seed; it refuses rather than \
         wrap a stock rustc), then seal it with scripts/promote-toolchain.sh"
    };
}

/// The from-source build, for a remedy to name after "from source" (`trust_from_source!`).
pub const TRUST_FROM_SOURCE: &str = trust_from_source!();

/// The three ways to get a Trust toolchain, for a caller to append verbatim. The package
/// manager leads (`aterm pkg doctor` names the store it filled); building from source is
/// the developer alternative.
pub const TRUST_TOOLCHAIN_REMEDIES: &str = concat!(
    "aterm pkg install trust   (then `aterm pkg doctor` to confirm the store)\n\
     or, from source: ",
    trust_from_source!(),
    "\n\
     or point TRUST_STAGE2_BIN at an existing stage2 bin dir"
);

/// The `targo` build driver from the stage2 tool dir. Every native-lane build and metadata
/// query goes through it, never a PATH `cargo` (a rustup shim with nothing behind it).
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
    /// Release version being cut, e.g. "0.2.0" — drives the tag gates.
    pub version: String,
    /// This cut publishes nowhere — `--dry-run` (no uploads at all). The
    /// channel-version gate is inapplicable then, because the public channel is
    /// not the destination and cannot be expected to carry this version.
    ///
    /// DERIVED FROM THE CLI FLAGS, never from the environment: an exported
    /// variable could silently disable the gate for a REAL cut.
    pub offline: bool,
    /// Only a `--dry-run` may execute from a cutter older than the tree.
    pub allow_stale_cutter: bool,
    /// A real cut's published commit, already placed in the cut tree
    /// ([`place_published`]); `None` for a dry run, which builds the checkout as
    /// it stands.
    pub published: Option<PublishedCheckout>,
}

/// What the gates learned, for the cut transcript's `gates` lines. Informational: any
/// failing gate already returned `Err`.
pub struct GateReport {
    /// Short (8-hex) HEAD sha for the banner.
    pub head_short: String,
    /// The gh account name, when it could be parsed from `gh auth status`.
    pub gh_account: Option<String>,
    /// Free disk in GiB at gate time.
    pub free_disk_gib: u64,
    /// `Some(version)` when the public update channel's source tree was read and
    /// carries exactly this version; `None` when there is no channel configured,
    /// no manifest on it yet, or the gate was explicitly skipped.
    pub channel_version: Option<String>,
    /// How many running processes the staging-liveness gate read — the evidence
    /// behind "nothing runs out of a cut staging bundle".
    pub processes_checked: usize,
    /// The published commit this cut releases — `None` for a dry run.
    pub published: Option<PublishedCheckout>,
}

/// Run every gate that depends on the cut's moment, in the transcript's order; the first
/// failure wins. Side-effect-free: it reads origin and the public channel and changes
/// neither.
///
/// Not here, because they depend only on the tree, and the cut's release judged that
/// tree before anything was staged (`ship release`'s step 4) and `verify release` ran
/// the tree-wide checks again over it: R's commit-only gates — the changelog, the
/// predecessor's handoff fixtures, the handoff policy, the locked metadata — and the
/// `release` check set (the handoff fixture guard's tests and the L0 censuses among
/// them). The cut reads that judgment instead (`publish::verified_release_bundle`: the
/// journal's `commit-gates` fact for R, and the pass record keyed by R's tree) and refuses without
/// it, so a cut by hand fails closed rather than skipping them. The two of them that
/// also read origin — the predecessor's handoff fixtures and the handoff policy, whose
/// predecessor is the newest release tagged on origin — the cut asks again itself
/// ([`handoff_fixture_gate_at`], [`handoff_policy_gate_at`]).
///
/// `git` and `tree` are the tree the cut releases — the cut tree for a real cut, the
/// checkout as it stands for a dry run; `state` is the operator's
/// checkout, whose `dist/` the cut reads and writes.
pub fn run_all(
    git: &dyn GitRunner,
    tree: &Path,
    state: &Path,
    opts: &GateOpts,
) -> Result<GateReport> {
    host_gate()?;
    // Before `clean_tree`, which would call a submodule left behind by a plain
    // `git pull` "uncommitted changes": the right fix names the submodule.
    submodules_at_gitlinks(git)?;
    clean_tree(git)?;
    // A real cut releases the published commit: `place_published` put the cut tree
    // there BEFORE this whole function, ahead of every file read. What is left
    // here is the ASSERTION that it worked. A dry run builds the checkout as it
    // stands.
    let head = rev_parse(git, "HEAD")?;
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
    // The tree is proven; now prove the binary proving it.
    own_cutter_gate(git, &head, opts.allow_stale_cutter)?;
    tag_free(git, &opts.version)?;
    let gh_account = gh_auth()?;
    let free_disk_gib = disk_gate(tree)?;
    let processes_checked =
        staged_bundle_liveness_gate(&state.join("dist"), &crate::bundle::running_processes)?;
    // Last: the only gate that talks to the public channel, after every cheap local refusal.
    let channel_version = channel_version_gate(tree, &opts.version, opts.offline)?;
    Ok(GateReport {
        head_short: head.chars().take(8).collect(),
        gh_account,
        free_disk_gib,
        channel_version,
        processes_checked,
        published: opts.published.clone(),
    })
}

/// Refuse a release whose committed handoff policy (`publish/handoff-policy.toml`) the
/// producers it is meant for could not follow, judged on the commit before anything is
/// staged (`ship release`'s step 4, `ship check`).
///
/// The bundle step copies the file into the `.app`, and every older aterm handing its
/// sessions to this release reads it there. A producer ignores a file it cannot interpret,
/// so a misspelt key would ship as a policy nobody follows. The cutter therefore reads the
/// commit's blob strictly ([`crate::bundle::handoff_policy_of_text`], the rule
/// [`crate::bundle::handoff_policy_for_cut`] applies): schema 1, only its keys and values,
/// never the reserved `seamless`, each rescue a `[[ranges]]` table with a reason, and the
/// top-level keys their hull.
///
/// Each rescue's range must name builds that can hand over to this release, and the file
/// must keep every rescue the `predecessor` (as [`handoff_fixture_gate_at`] finds it)
/// sealed, or retire it by name ([`sealed_policy_kept`]): a rescue stays while any build
/// it names may still be installed.
///
/// # Errors
/// The commit holds no policy, one producers could not follow, or one that drops what its
/// predecessor sealed without retiring it.
pub fn handoff_policy_gate_at(
    git: &dyn GitRunner,
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
    let predecessor = predecessor_in(git, &tree, version)?;
    sealed_policy_kept(git, &tree, predecessor.as_deref(), policy, &text)
}

/// What [`handoff_policy_gate_at`] judged: the policy a cut seals into its bundle,
/// and the rescues its predecessor sealed that it retires on purpose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CutHandoffPolicy {
    pub policy: aterm_update_core::handoff_policy::PolicyFile,
    /// The predecessor release and the file's `[[retired]]` entry, for each
    /// rescue that release sealed and this cut drops.
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

/// Refuse a policy whose rescues name no producer that can follow them or bind builds not
/// cut yet
/// ([`PolicyFile::names_producers`](aterm_update_core::handoff_policy::PolicyFile::names_producers)
/// over [`builds_handing_over`] in `tree`'s ledger; no build when nothing came before).
/// Refuse one that drops a rescue `predecessor` sealed
/// ([`PolicyFile::keeps`](aterm_update_core::handoff_policy::PolicyFile::keeps)) unless a
/// `[[retired]]` entry of `text` names that rescue's exact range with a reason and a date,
/// and refuse a `[[retired]]` entry for a rescue this file keeps. An entry for a range the
/// predecessor did not seal records an earlier retirement and is kept. The sealed policy is
/// the file at the commit origin's `v<release>` tag names ([`shipped_commit`]), read as a
/// producer reads it (top-level keys as one rescue when it lists none); a tree without the
/// file sealed none.
fn sealed_policy_kept(
    git: &dyn GitRunner,
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
    let shipped = shipped_commit(git, release)?;
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

/// Every build `ledger_text` claimed up to `release`'s newest claim: the builds whose
/// producers can hand their sessions to the release that succeeds it (a cut claims past
/// all of them, and a claim between `release`'s and the cut never shipped). Abandoned
/// claims count too: the ledger says they were claimed, not that none was installed.
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

/// The commit origin's `refs/tags/v<release>` names (the release that shipped, where
/// [`predecessor_release`] found it), fetched into FETCH_HEAD alone when this repository
/// lacks it. Never this clone's tag of that name: an abandon deletes origin's tag and the
/// abandoning clone's only, a fetch never moves a tag a clone holds, and a recut re-points
/// origin's, so a clone's tag can name a commit the shipped release was not cut from.
fn shipped_commit(git: &dyn GitRunner, release: &str) -> Result<String> {
    let tag = format!("refs/tags/v{release}");
    let listing = git_ok(
        git,
        &[
            "ls-remote",
            "--tags",
            "origin",
            &tag,
            &format!("{tag}^{{}}"),
        ],
    )?;
    let Some(commit) = tag_commits(&listing.stdout_utf8()).remove(&format!("v{release}")) else {
        return Err(Error::new(format!(
            "handoff-policy gate: origin no longer carries v{release}, the release this cut \
             succeeds; nothing was claimed — run it again"
        )));
    };
    let at = format!("{commit}^{{commit}}");
    let present = || {
        git.git(&["rev-parse", "--verify", "-q", &at])
            .map(|out| out.success())
    };
    if !present()? {
        // `--refmap=` ignores the configured refspecs, so not even a clone that
        // maps origin's tags onto its own has one moved by this fetch.
        git_ok(
            git,
            &["fetch", "-q", "--no-tags", "--refmap=", "origin", &tag],
        )?;
        if !present()? {
            return Err(Error::new(format!(
                "handoff-policy gate: origin's v{release} named {} when listed, and fetching \
                 it did not bring that commit (origin moved the tag); nothing was claimed — \
                 run it again",
                short(&commit)
            )));
        }
    }
    Ok(commit)
}

/// No live bundle under the cut's `rm -rf`: refuses, pre-claim, when any process executes
/// out of a cut staging directory under `dist` ([`crate::bundle::is_staging_dir_name`]),
/// naming the pids and the path, or when the process list cannot be read. Returns how many
/// processes it read.
///
/// The cut deletes and replaces staging bundles (`bundle::prepare_staging`,
/// `bundle::prune_staging_dirs`, which ask again when deleting; this is the early refusal).
/// A bundle deleted under a live process leaves it at a path that no longer resolves, and
/// macOS answers a code identity it cannot build by replacing the stored requirement,
/// resetting the TCC grants of every copy of aterm on the Mac. `lister` is injected; `None`
/// ("could not look") refuses, since it can never license a delete.
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

// ---------------------------------------------------------------------------
// the published commit
// ---------------------------------------------------------------------------

/// Where the publication engine keeps its ledger: `$PUBLICATION_ENGINE/mappings.json`,
/// default `~/publication/mappings.json` (a location, not a skip switch; `ship release`
/// reads the same). `pub publish` writes a row there for every source publication: the
/// dev commit it exported, keyed by its full sha.
#[must_use]
pub fn publication_mappings_path() -> PathBuf {
    env::var_os("PUBLICATION_ENGINE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join("publication")))
        .unwrap_or_else(|| PathBuf::from("publication"))
        .join("mappings.json")
}

/// The dev commit the newest aterm source publication exported, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedSource {
    /// Full dev sha — the `mappings.aterm.<sha>` key.
    pub commit: String,
    /// The row's `verified_at`, verbatim (RFC 3339, so it sorts as text).
    pub verified_at: String,
}

/// The newest `mappings.aterm` row by `verified_at` in the engine's ledger text.
///
/// # Errors
/// Unparseable JSON, no `mappings.aterm` object, or no row with a full-hex key.
pub fn newest_published_source(mappings_json: &str) -> Result<PublishedSource> {
    let value: aterm_json::Value = aterm_json::from_str(mappings_json)
        .map_err(|e| Error::new(format!("the publication ledger is not JSON: {e}")))?;
    let rows = value
        .get("mappings")
        .and_then(|m| m.get("aterm"))
        .and_then(aterm_json::Value::as_object)
        .ok_or_else(|| Error::new("the publication ledger has no `mappings.aterm` rows"))?;
    rows.iter()
        .filter(|(sha, _)| aterm_codec::hex::is_hex(sha, 40))
        .map(|(sha, row)| PublishedSource {
            commit: sha.clone(),
            verified_at: row
                .get("verified_at")
                .and_then(aterm_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
        .max_by(|a, b| a.verified_at.cmp(&b.verified_at))
        .ok_or_else(|| Error::new("the publication ledger has no aterm row with a commit key"))
}

/// The commit a real cut releases: the newest aterm row of the engine's ledger
/// ([`publication_mappings_path`], [`newest_published_source`]).
///
/// # Errors
/// An unreadable or rowless ledger is a refusal: a real cut releases the commit
/// `pub publish` recorded, and that ledger is the only record of it.
pub fn published_source() -> Result<PublishedSource> {
    let ledger = publication_mappings_path();
    let text = fs::read_to_string(&ledger).map_err(|e| {
        Error::new(format!(
            "cannot read the publication ledger {} ({e}) — a real cut releases the commit \
             `pub publish` recorded there. Pull the engine (`git -C {} pull \
             --ff-only`) or point PUBLICATION_ENGINE at its checkout. Nothing was claimed.",
            ledger.display(),
            ledger.parent().unwrap_or(&ledger).display()
        ))
    })?;
    newest_published_source(&text).map_err(|e| Error::new(format!("{}: {e}", ledger.display())))
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

/// Put the cut tree ([`cut_tree_path`]) at `source`, detached, so everything the cut reads
/// and builds (Cargo.toml's version, the changelog's notes, the proof snapshot's sources,
/// the cutter itself) is exactly the commit `pub publish` exported and the public source
/// release carries.
///
/// Not main's tip: the binary and the public source are one release under one tag, so
/// they must be one tree, and a moving tip is one no gate passed; peer pushes ship in the
/// next release. Not this checkout: agent sessions share it, so a detached HEAD there
/// parks their work on old code, and one that "fixes" it mid-cut breaks the build.
///
/// Preconditions: `origin` reachable (a real cut is never offline), `source` present after
/// the fetch, and `source` on `origin/main` (`pub stage` exports main's history and the
/// claim lands on main).
///
/// # Errors
/// Each precondition above, [`place_cut_tree`]'s, and any git failure.
pub fn place_published(
    git: &dyn GitRunner,
    repo: &Path,
    source: &PublishedSource,
) -> Result<PublishedCheckout> {
    git_ok(git, &["fetch", "origin", "main"])
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
    let on_main = git.git(&["merge-base", "--is-ancestor", commit, "origin/main"])?;
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
    let range = format!("{commit}..origin/main");
    let main_ahead = git_ok(git, &["rev-list", "--count", &range])?
        .stdout_utf8()
        .trim()
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

/// The cut tree for the checkout at `repo`: `<parent>/<name>-cut.noindex`, a linked
/// worktree of the same repository that belongs to the cutter.
///
/// Beside the checkout, never inside it: cargo reads `.cargo/config.toml` from its working
/// directory and every ancestor, merging arrays such as `rustflags`, so a tree under
/// `dist/` or `target/` would build with this checkout's configuration folded in.
/// `.noindex` keeps Spotlight out of its release target trees.
///
/// # Errors
/// A checkout at the filesystem root has no parent to put a sibling in.
pub fn cut_tree_path(repo: &Path) -> Result<PathBuf> {
    match (repo.parent(), repo.file_name()) {
        (Some(parent), Some(name)) => {
            let mut leaf = name.to_os_string();
            leaf.push("-cut.noindex");
            Ok(parent.join(leaf))
        }
        _ => Err(Error::new(format!(
            "{} has no parent directory to hold the cut tree beside it",
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

/// Put the cut tree ([`cut_tree_path`]) at `commit`, detached, creating it as a linked
/// worktree of `repo`'s repository when absent. `git` runs in `repo`, the operator's
/// checkout, which this never changes. Every checkout it makes is [`BYTE_EXACT`], whatever
/// the operator's config. A resume puts it back at its journaled release commit the same
/// way.
///
/// # Errors
/// A path that exists but is not a worktree of this repository (it is never touched),
/// a cut tree with changes (the cutter never discards work it did not make), and any
/// git failure.
pub fn place_cut_tree(git: &dyn GitRunner, repo: &Path, commit: &str) -> Result<CutTree> {
    let path = cut_tree_path(repo)?;
    if !path.exists() {
        // A tree someone deleted by hand is still registered; `worktree add` refuses a
        // registered path until the stale entry is pruned.
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
        let out = command("git")
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
    // Moved or not: a tree created before its commit carried a submodule, or one an
    // earlier failed alignment left behind, is brought to the gitlinks here too.
    align_submodules(&tree)?;
    require_clean_cut_tree(&path)?;
    Ok(CutTree { path, moved })
}

/// Judge the cut tree both before and after a move. A removed ignore rule can
/// expose files without establishing who wrote them, so preserve them and refuse.
fn require_clean_cut_tree(path: &Path) -> Result<()> {
    // THE ONE CLEAN-TREE JUDGE ([`dirty_paths`]'s, which `clean_tree` asks of this tree
    // right after): never `git status --porcelain`, which also reads
    // `.git/info/exclude` and `core.excludesFile` and so passed what that judge then
    // refused. A submodule not populated is the alignment's to fix (below), not work.
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

/// `path`, once its `HEAD` is proven to be `commit`.
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

/// Where the whole-tree Trust lane files its facts: `<git common dir>/aterm-verify/receipts`,
/// the one store every worktree of the repository shares.
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

/// This process's pinned Trust toolchain, used by release builds and the default
/// explicit Trust verdict: `<stage2 bin dir> trustc <commit-hash>`. The stage2
/// ([`trust_stage2_bin`]) is canonical; the hash comes from `trustc -vV`, or is
/// `unknown` when it names none. A cut signs only bytes this toolchain compiled
/// (`publish::verified_release_bundle`): one compiler, named by the bytes.
///
/// # Errors
/// No toolchain, or a trustc that does not answer `-vV`.
pub fn cut_toolchain() -> Result<String> {
    toolchain_at(&trust_stage2_bin()?)
}

/// The `trustc` commit-hash a [`cut_toolchain`] identity names (`unknown` when it
/// names none): what a bundle's build record (`artifact::Complete::trustc`) is
/// compared with.
#[must_use]
pub fn toolchain_trustc(toolchain: &str) -> &str {
    toolchain
        .rsplit_once(" trustc ")
        .map_or("unknown", |(_, commit)| commit)
}

/// [`cut_toolchain`]'s spelling for the toolchain whose `bin/` is `stage2`
/// (canonical, as the gate records it).
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

/// Where `ship release` keeps its [`VerifiedRecord`]s:
/// `<git common dir>/aterm-release/verified/<tree>`, shared by every worktree of the clone
/// (so `publish/pre-promote`'s throwaway worktree reads them too).
pub const VERIFIED_DIR: &str = "verified";

/// The record that a release's checks passed: written by `ship release` when `verify
/// release --app <bundle>` exits 0 in the clone at R, filed under R's tree, and naming the
/// executable those checks judged. Read by [`verified`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VerifiedRecord {
    /// The commit `verify release` ran in.
    pub commit: String,
    /// SHA-256 of the executable it judged (`aterm.app/Contents/MacOS/aterm`).
    pub executable: String,
}

/// `(the record's path, commit's tree)` for `commit`.
fn verified_path(git: &dyn GitRunner, commit: &str) -> Result<(PathBuf, String)> {
    let tree = rev_parse(git, &format!("{commit}^{{tree}}"))?;
    let path = crate::release::release_dir(git)
        .map_err(Error::new)?
        .join(VERIFIED_DIR)
        .join(&tree);
    Ok((path, tree))
}

/// Record that `verify release` passed for `commit`'s tree on the executable
/// `macho_sha256` ([`VerifiedRecord`]): written whole, flushed and renamed into place.
///
/// # Errors
/// git cannot name the tree or the common dir; any write.
pub fn record_verified(git: &dyn GitRunner, commit: &str, macho_sha256: &str) -> Result<PathBuf> {
    let (path, _) = verified_path(git, commit)?;
    let record = VerifiedRecord {
        commit: commit.to_string(),
        executable: macho_sha256.to_string(),
    };
    let text = aterm_toml::to_string(&record)
        .map_err(|e| Error::new(format!("serialize the verified record: {e}")))?;
    let dir = path.parent().unwrap_or(&path);
    let tmp = dir.join(format!(".verified.{}.tmp", std::process::id()));
    let write = || -> std::io::Result<()> {
        fs::create_dir_all(dir)?;
        let mut file = fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut file, text.as_bytes())?;
        crate::durable::sync_all(&file)?;
        fs::rename(&tmp, &path)?;
        crate::durable::sync_dir(dir)
    };
    write().map_err(|e| {
        let _ = fs::remove_file(&tmp);
        Error::new(format!("write {}: {e}", path.display()))
    })?;
    Ok(path)
}

/// Did these bytes, from this tree, pass every check a release requires? The one judge the
/// release's resume, the cut, its sign step and `ship check` ask: `commit`'s tree holds a
/// [`VerifiedRecord`] naming exactly the executable `macho_sha256`. Returns `(true, the
/// transcript line)` or `(false, what the store holds instead)`; an unreadable or foreign
/// file is no pass.
///
/// # Errors
/// git cannot name the tree, or the record cannot be read at all: no answer either way.
pub fn verified(git: &dyn GitRunner, commit: &str, macho_sha256: &str) -> Result<(bool, String)> {
    let (path, tree) = verified_path(git, commit)?;
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((
                false,
                format!(
                    "no record that `verify release` passed for {}'s tree {}",
                    short(commit),
                    short(&tree)
                ),
            ));
        }
        Err(e) => return Err(Error::new(format!("read {}: {e}", path.display()))),
    };
    let Ok(record) = aterm_toml::from_str::<VerifiedRecord>(&text) else {
        return Ok((
            false,
            format!("{} is not a record this cutter reads", path.display()),
        ));
    };
    if record.executable != macho_sha256 {
        return Ok((
            false,
            format!(
                "`verify release` passed for {}'s tree on the executable {}, not {}",
                short(commit),
                short(&record.executable),
                short(macho_sha256)
            ),
        ));
    }
    Ok((
        true,
        format!(
            "verify release: PASSED for {}'s tree {} on the executable {} (run in {})",
            short(commit),
            short(&tree),
            short(macho_sha256),
            short(&record.commit)
        ),
    ))
}

/// `ship check`'s `verify release` line for `commit`: this clone's release journal's bundle
/// of it ([`crate::release::Journal::artifact`]; the journal lives in the git common dir,
/// so `publish/pre-promote`'s throwaway worktree reads it too) and what [`verified`] says
/// of its executable. No bundle of `commit` built here refuses.
///
/// # Errors
/// Not verified, or not built yet, with the remedy; and a record or journal that cannot
/// be read.
pub fn verify_gate_at(git: &dyn GitRunner, commit: &str) -> Result<String> {
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
    match verified(git, commit, &artifact.macho_sha256)? {
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

/// What one `ship check` gate answered. Typed: the exit code, the release's preflight
/// and the tests read the variant, never the words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It ran and passed: its transcript line.
    Pass(String),
    /// It ran, and a release of the commit would be refused: why, and the remedy.
    Refuse(Refusal),
    /// It decided nothing (a missing tool, an engine that is not current, another
    /// commit's cutter), so it is never a pass: why, and the fix.
    CouldNotRun(Refusal),
}

/// A cut gate's error text as a [`Refusal`]: its first line the why, its `fix:` line
/// the remedy (empty when the why carries its own), every other line detail.
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
    /// Every gate asked, in order: the cut's (the cutter identity first), then the
    /// publication engine's export guards, then the wasm32 engine.
    pub gates: Vec<PrecheckGate>,
}

impl Precheck {
    /// The commit `rev` names and the release its `Cargo.toml` declares, no gate
    /// asked yet.
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

    /// `ship check`'s exit: 0 every gate passed, 1 one would refuse, 3 none refused
    /// but one could not run (nothing it judges was decided).
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        if !self.refused().is_empty() {
            1
        } else if !self.not_run().is_empty() {
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

    /// One gate's lines: `ok` with its line, or `REFUSE` / `NOT RUN` with why, the
    /// detail and the fix.
    #[must_use]
    pub fn gate_lines(gate: &PrecheckGate) -> String {
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
            out.push_str(&format!("          {line}\n"));
        }
        if !r.remedy.is_empty() {
            out.push_str(&format!("          fix: {}\n", r.remedy));
        }
        out
    }

    /// What `ship check` prints before its verdict: the header, then each gate (it
    /// prints them as each gate ends).
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

/// `ship check --cut` (and a bare `ship check`): for one commit, with no build of aterm,
/// the pre-claim gates of a real cut that read only the commit and this clone's records,
/// in the cut's order, so the source is never published ahead of a binary that cannot
/// follow it:
///
/// * the cutter identity ([`own_cutter`]) first: when this binary is not the commit's own
///   cutter the rest are not run, since their answers would be another tree's rules;
/// * [`changelog_gate_at`], [`handoff_fixture_gate_at`], [`handoff_policy_gate_at`];
/// * [`verify_gate_at`]: this clone's bundle of it passed `verify release`.
///
/// Each reads the commit's objects, never a checkout, and every gate after the identity
/// runs whatever the one before said, so one check names everything missing.
/// `publish/pre-promote` runs it for the commit `pub promote` is about to publish, with the
/// commit's own cutter (`ATERM_RELEASE_CUTTER` from a running release, else one it builds
/// in a throwaway worktree).
///
/// It is not the cut's whole preflight: what needs a build (the fixture
/// guard's test run, the L0 censuses) is `verify release`'s, whose pass record
/// the verify gate reads; the tag and the published
/// channel (the tag and channel version gates) and the cut's own moment (the
/// lock, the roster, the credentials, the disk) are the cut's, which asks them
/// itself.
///
/// # Errors
/// `rev` names no commit, or the commit's Cargo.toml carries no release version.
pub fn precheck(git: &dyn GitRunner, rev: &str) -> Result<Precheck> {
    precheck_with(git, rev, crate::cutter::running())
}

/// [`precheck`] by a cutter whose verdict ([`crate::cutter::running`]) is `stamp`.
fn precheck_with(
    git: &dyn GitRunner,
    rev: &str,
    stamp: std::result::Result<&str, &crate::cutter::NotOwn>,
) -> Result<Precheck> {
    let mut found = Precheck::of(git, rev)?;
    let (commit, version) = (found.commit.clone(), found.version.clone());
    let identity = match own_cutter_of(git, stamp, &commit) {
        Ok(note) => note,
        Err(not) => {
            // another commit's build is answered by the commit's own; a binary that is no
            // commit's build names its own remedy
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
            outcome: answer(
                changelog_gate_at(git, &commit, &version).map(|(section, found)| {
                    format!("CHANGELOG [{section}]: {} entries, no '''", found.entries)
                }),
            ),
        },
        PrecheckGate {
            name: "handoff fixtures",
            outcome: answer(
                handoff_fixture_gate_at(git, &commit, &version)
                    .map(|found| handoff_fixtures_line(found.as_ref())),
            ),
        },
        PrecheckGate {
            name: "handoff policy",
            outcome: answer(
                handoff_policy_gate_at(git, &commit, &version)
                    .map(|found| format!("handoff policy a cut seals into the bundle: {found}")),
            ),
        },
        PrecheckGate {
            name: "verify release",
            outcome: answer(verify_gate_at(git, &commit)),
        },
    ];
    Ok(found)
}

/// The line for what [`handoff_fixture_gate_at`] found: `ship check`'s, `ship
/// release`'s step 4 and the cut's.
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

/// Prove the public channel's source tree already carries the version being cut: the I/O
/// shell (the channel slug from the local manifest, the channel's `Cargo.toml` at `main`)
/// around [`channel::check_channel_version`].
///
/// Fetch failures fail CLOSED. An unreachable channel is indistinguishable from a
/// channel that disagrees, and the whole purpose of the gate is to stop guessing
/// about the channel's contents. `offline` (from `--dry-run`, never from
/// the environment) is the ONLY opt-out, and it cannot apply to a real cut.
///
/// In particular a bare 404 is NOT taken as "no manifest": GitHub returns 404 for an
/// unauthorized repository as well as a missing file, so an absent channel token
/// would otherwise disable this gate silently. The 404 path probes the repository
/// itself and only skips when the repository is demonstrably readable.
pub fn channel_version_gate(repo: &Path, version: &str, offline: bool) -> Result<Option<String>> {
    let local = fs::read_to_string(repo.join("Cargo.toml"))
        .map_err(|e| Error::new(format!("cannot read workspace Cargo.toml: {e}")))?;
    let Some(slug) = channel::update_channel_slug(&local)? else {
        return Ok(None);
    };
    if offline {
        return Ok(None);
    }

    let out = channel_api(&format!("repos/{slug}/contents/Cargo.toml?ref=main"))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if !is_not_found(&err) {
            return Err(Error::new(format!(
                "cannot read Cargo.toml from the public channel {slug}: {}",
                err.trim()
            )));
        }
        // A 404 also answers a repository the credential cannot read, so ask whether the
        // repository is readable at all before taking it as "no manifest".
        let probe = channel_api(&format!("repos/{slug}"))?;
        if !probe.status.success() {
            return Err(Error::new(format!(
                "the public channel {slug} is not readable with the available credential, so \
                 this cut cannot confirm it carries v{version} (GitHub's 404 means missing \
                 or unauthorized).\n\
                 fix:  provide ~/.secrets/gh_access_token_alabsystems, or cut with --dry-run"
            )));
        }
        // Repo readable, file absent: the genuine empty-channel/first-publish case.
        return Ok(None);
    }

    let body = String::from_utf8_lossy(&out.stdout).to_string();
    match channel::check_channel_version(version, &body)? {
        channel::ChannelVersion::Agrees => Ok(Some(version.to_string())),
        channel::ChannelVersion::NoManifest => Ok(None),
    }
}

/// One `gh api` call against the public channel, credentialed for the release org: the
/// channel is a different org than the dev remote, whose `gh auth` account can read it
/// (a pull) but has no push there; the release org's token, when present, reads it as
/// the publisher. The token goes in the environment, never argv, so no process listing
/// shows it.
fn channel_api(path: &str) -> Result<std::process::Output> {
    let mut command = command("gh");
    command
        .arg("api")
        .arg(path)
        .args(["-H", "Accept: application/vnd.github.raw"]);
    if let Some(token) = crate::publish::channel_token() {
        command.env("GH_TOKEN", token);
    }
    command
        .output()
        .map_err(|e| Error::new(format!("cannot run gh to read the public channel: {e}")))
}

/// Does this `gh` stderr report a 404 (`gh: Not Found (HTTP 404)` plus the JSON body)?
/// Matching loosely is safe: the caller probes the repository before acting on it.
fn is_not_found(stderr: &str) -> bool {
    stderr.contains("404") || stderr.contains("Not Found")
}

/// Prove the committed Cargo.lock resolves the workspace with no rewrite or network
/// access, so a stale lock fails before the claim rather than dirtying the tree during
/// the build.
pub fn locked_metadata_gate(repo: &Path) -> Result<()> {
    let lock_path = repo.join("Cargo.lock");
    let lock_before = fs::read(&lock_path).map_err(|error| {
        Error::new(format!(
            "read committed Cargo.lock before release metadata check: {error}"
        ))
    })?;
    let targo = resolve_targo()?;
    let out = command(&targo)
        .args(["metadata", "--locked", "--offline", "--format-version", "1"])
        .current_dir(repo)
        .output()
        .map_err(|error| Error::new(format!("failed to run locked targo metadata: {error}")))?;
    let lock_after = match fs::read(&lock_path) {
        Ok(bytes) => bytes,
        Err(read_error) => {
            fs::write(&lock_path, &lock_before).map_err(|restore_error| {
                Error::new(format!(
                    "Cargo.lock disappeared during locked metadata ({read_error}) and restoring \
                     its exact prior bytes failed: {restore_error}"
                ))
            })?;
            return Err(Error::new(format!(
                "Cargo.lock disappeared during locked metadata ({read_error}); exact prior bytes \
                 were restored"
            )));
        }
    };
    if lock_after != lock_before {
        fs::write(&lock_path, &lock_before).map_err(|error| {
            Error::new(format!(
                "locked Cargo metadata changed Cargo.lock and restoring its exact prior bytes \
                 failed: {error}"
            ))
        })?;
        return Err(Error::new(
            "locked Cargo metadata attempted to rewrite Cargo.lock; exact prior bytes were \
             restored — refresh and commit the lock before cutting"
                .to_string(),
        ));
    }
    if !out.status.success() {
        return Err(Error::new(format!(
            "Cargo.lock is not an exact offline resolution of Cargo.toml — refresh and commit \
             it before cutting: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// Releases are cut only on an arm64 Mac. The compile-time cfg is the host check: the ship
/// binary is always built on the cutting machine by the `targo --unverified ship` alias,
/// never cross-compiled or installed, so target is host by construction.
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

/// Clean tree: the release commit is exactly the published commit plus the changelog roll
/// and ledger line, and the claim checks this tree out onto it, so stray local edits are
/// refused up front. Judged by [`crate::clean`], the one clean-tree judge `ship release`
/// asks of its clone too, so no step calls a tree clean that another calls dirty.
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

/// Every submodule initialised and checked out at exactly the commit `HEAD` records for it
/// (`aterm-link` path-depends on crates inside `vendor/astream`). Read-only.
///
/// `clean_tree` cannot see the worst case: an uninitialised submodule is not dirty to `git
/// status`, and cargo then fails minutes into the build. A submodule on another commit is
/// dirty, but its usual cause is a `git pull` that moved the gitlink and left the checkout,
/// so this names the submodule and the right fix.
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

/// The `git submodule status` lines that are not at their gitlink: every line
/// whose status column is not a space. Pure, so the parse is a test.
pub fn submodules_off_gitlink(status: &str) -> Vec<&str> {
    status
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with(' '))
        .collect()
}

/// After [`place_cut_tree`] creates or moves the cut tree: every submodule to the commit
/// the tree's new `HEAD` records, from its configured remote when the commit is not local.
///
/// A linked worktree brings no submodule checkout, and `checkout --detach` moves the
/// gitlink and never the checkout, so without this the published commit would build
/// against no astream, or the previous commit's. (A claim needs no such step: the release
/// commit's gitlinks are the published commit's.) Fails loudly, naming what is left.
pub fn align_submodules(git: &dyn GitRunner) -> Result<()> {
    let update = ["submodule", "update", "--init", "--recursive", "--checkout"];
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

/// Every submodule pin is on its declared branch: the commit each gitlink of `HEAD`
/// records is an ancestor of the branch `.gitmodules` declares for it
/// (`submodule.<name>.branch`, else the remote's `HEAD`) as the submodule's `origin` has
/// it now.
///
/// A bus change lands in astream first, and landing is reaching astream's main. A pin to a
/// pushed side branch passes every offline check, ships code astream's main never carried,
/// and leaves this commit unbuildable once the branch is gone, so the release asks the
/// submodule's remote. A failed fetch, or an ancestry git cannot decide, refuses.
pub fn submodules_on_declared_branch(git: &dyn GitRunner) -> Result<()> {
    let listing = git_ok(git, &["ls-tree", "-r", "HEAD"])?.stdout_utf8();
    let links = gitlinks(&listing);
    if links.is_empty() {
        return Ok(());
    }
    // `submodule.<name>.path <path>`; no `.gitmodules`, or no entry, lists nothing.
    let declared = git
        .git(&[
            "config",
            "-f",
            ".gitmodules",
            "--get-regexp",
            r"^submodule\..*\.path$",
        ])?
        .stdout_utf8();
    let mut off = Vec::new();
    for (pin, path) in &links {
        let Some(name) = declared.lines().find_map(|l| {
            let (key, at) = l.split_once(' ')?;
            (at.trim() == path)
                .then(|| key.strip_prefix("submodule.")?.strip_suffix(".path"))
                .flatten()
        }) else {
            return Err(Error::new(format!(
                "the gitlink {path} has no .gitmodules entry, so the branch it must be on is \
                 unknown"
            )));
        };
        let branch = git.git(&[
            "config",
            "-f",
            ".gitmodules",
            "--get",
            &format!("submodule.{name}.branch"),
        ])?;
        let (branch, source) = if branch.success() {
            let branch = branch.stdout_utf8().trim().to_string();
            // `--no-tags` disables automatic tag following, but an unqualified
            // name still fetches a same-named tag if the branch is absent.
            let source = format!("refs/heads/{branch}");
            (branch, source)
        } else {
            ("HEAD".to_string(), "HEAD".to_string())
        };
        let fetched = git.git(&[
            "-C",
            path,
            "fetch",
            "--quiet",
            "--no-tags",
            "origin",
            &source,
        ])?;
        if !fetched.success() {
            return Err(Error::new(format!(
                "cannot fetch {path}'s declared branch {branch} from its origin (exit {}): {}, \
                 so whether its pin {pin} is on it is unknown",
                fetched.status,
                fetched.stderr_utf8().trim()
            )));
        }
        let on = git.git(&["-C", path, "merge-base", "--is-ancestor", pin, "FETCH_HEAD"])?;
        match on.status {
            0 => {}
            1 => off.push(format!("{path} at {pin} is not on {branch}")),
            n => {
                return Err(Error::new(format!(
                    "git cannot tell whether {path}'s pin {pin} is on {branch} (exit {n}): {}",
                    on.stderr_utf8().trim()
                )));
            }
        }
    }
    if off.is_empty() {
        return Ok(());
    }
    Err(Error::new(format!(
        "submodule pin(s) on no commit their declared branch carries — a release ships only \
         what each repository's own branch carries:\n  {}",
        off.join("\n  ")
    )))
}

/// What a claim commit changes on top of the commit it claims: its ledger line and
/// its changelog roll (`ledger::reserve` makes it, `ledger::claim` lands it), and
/// nothing else.
pub const CLAIM_FILES: [&str; 2] = [crate::changelog::CHANGELOG_FILE, crate::ledger::LEDGER_FILE];

/// The cutter is its commit's own. Every other pre-claim gate proves something about the
/// tree and nothing about the binary doing the proving: a cutter built from an older tree
/// validates against its own older rules and passes. So the running binary is judged for
/// the commit it is the build of ([`crate::cutter::running`]) and acts for `commit` only
/// when it is that commit, or `commit` is the claim commit made on top of it (first parent
/// the stamp, nothing but [`CLAIM_FILES`] differing, the one shape `ledger::reserve`
/// makes). `targo --unverified ship release` re-runs itself as the release commit's own
/// cutter before it judges anything (`release.rs`).
///
/// # Errors
/// This binary is not `commit`'s cutter: why, and the remedy.
pub fn own_cutter(
    git: &dyn GitRunner,
    commit: &str,
) -> std::result::Result<String, crate::cutter::NotOwn> {
    own_cutter_of(git, crate::cutter::running(), commit)
}

/// [`own_cutter`] for a cutter whose verdict is `stamp`: the commit it is the build of,
/// or why it is none.
///
/// # Errors
/// As [`own_cutter`].
fn own_cutter_of(
    git: &dyn GitRunner,
    stamp: std::result::Result<&str, &crate::cutter::NotOwn>,
    commit: &str,
) -> std::result::Result<String, crate::cutter::NotOwn> {
    match stamp {
        Ok(stamp) => own_cutter_with(git, stamp, commit).map_err(|why| crate::cutter::NotOwn {
            why,
            fix: format!(
                "run it as `{}` from the release's clone (it re-runs itself as the release \
                     commit's own cutter), or as `targo --unverified ship <verb>` from a clean \
                     checkout of {commit} with its own target directory (cargo relinks the \
                     cutter from that tree before it runs it)",
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

/// [`own_cutter`] for a cutter that is the build of `stamp` — pure over the stamp, so
/// every case is a test rather than a rebuild.
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
    let parent = git
        .git(&["rev-parse", "--verify", "-q", &format!("{commit}^1")])
        .ok()
        .filter(crate::ledger::RunOut::success)
        .map(|out| out.stdout_utf8().trim().to_string());
    if parent.as_deref() == Some(stamp)
        && let Ok(out) = git.git(&["diff", "--name-only", "--no-renames", stamp, commit])
        && out.success()
    {
        let text = out.stdout_utf8();
        let changed: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if !changed.is_empty() && changed.iter().all(|p| CLAIM_FILES.contains(p)) {
            return Ok(format!(
                "cutter identity: built from {}, whose claim commit {} adds only its ledger \
                 line and changelog roll",
                short(stamp),
                short(commit)
            ));
        }
    }
    Err(format!(
        "this aterm-release binary was built from {stamp}, not {commit}"
    ))
}

/// [`own_cutter`] as a gate, with its remedy. Only a true `--dry-run` (`allow_stale`),
/// which publishes nowhere, proceeds with another commit's cutter — and says so,
/// because its results came from other code than the tree's.
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
/// Resume, recovery, abandon and yank do not enter [`run_all`], so each of those paths
/// uses this shared boundary before its first remote mutation.
///
/// # Errors
/// As [`own_cutter_gate`].
pub fn current_own_cutter_gate(git: &dyn GitRunner) -> Result<()> {
    own_cutter_gate(git, &rev_parse(git, "HEAD")?, false)
}

/// Tag vX.Y.Z must be absent locally and on origin: the publish step mints it, so a present
/// tag means this version was cut (or half-cut), and colliding with it would re-point a
/// published artifact. A tag on origin is a cut, so the next release is the next minor; a
/// tag only here is a leftover (an abandon deletes origin's), and the version can be cut
/// once it is gone.
pub fn tag_free(git: &dyn GitRunner, version: &str) -> Result<()> {
    let tag = format!("v{version}");
    // Local: `rev-parse -q --verify` exits 0 iff the ref exists, so here a non-zero exit
    // is the happy path.
    let local = git.git(&["rev-parse", "-q", "--verify", &format!("refs/tags/{tag}")])?;
    let remote = git_ok(
        git,
        &["ls-remote", "--tags", "origin", &format!("refs/tags/{tag}")],
    )?;
    if !remote.stdout_utf8().trim().is_empty() {
        return Err(Error::new(format!(
            "tag {tag} is on origin: v{version} was cut. The next release is v{}: `pub bump \
             aterm --minor --write` on main, commit and push, then `pub stage aterm` and `pub \
             publish aterm`, then cut",
            crate::publish::bump_minor_release(version)?
        )));
    }
    if local.success() {
        return Err(Error::new(format!(
            "tag {tag} exists locally but not on origin: `git tag -d {tag}`, then cut again"
        )));
    }
    Ok(())
}

/// The changelog gate (changelog.rs) for a commit, read from its objects: the section a
/// cut of it ships (the version's own section when the commit already carries one, a
/// recut; else `[Unreleased]`) has a non-empty body and no `'''`. Returns that section
/// with what the gate found.
///
/// # Errors
/// The commit holds no changelog, or the cut's changelog refusal.
pub fn changelog_gate_at(
    git: &dyn GitRunner,
    commit: &str,
    version: &str,
) -> Result<(String, changelog::GateSummary)> {
    let text = GateTree::Commit { git, commit }.text(changelog::CHANGELOG_FILE)?;
    let section = if changelog::has_section(&text, version) {
        version
    } else {
        "Unreleased"
    };
    changelog_section_gate(&text, section).map(|found| (section.to_string(), found))
}

/// The one rule [`changelog_gate_at`] applies to a changelog's text.
fn changelog_section_gate(text: &str, section: &str) -> Result<changelog::GateSummary> {
    if section == "Unreleased" {
        changelog::gate_unreleased(text)
    } else {
        changelog::gate_section(text, section)
    }
}

/// Where each shipped release's handoff producer bytes are checked in, one
/// directory per release (`v<version>/`) and one per desk inside it.
pub const HANDOFF_FIXTURE_ROOT: &str = "crates/aterm-gui/tests/fixtures/handoff";

/// How to add a release's fixtures — the one place the steps are written down,
/// so the refusal points here rather than restating them.
pub const HANDOFF_FIXTURE_README: &str = "crates/aterm-gui/tests/fixtures/handoff/README.md";

/// The guard that runs every fixture through the current consumer, and whose
/// `PINNED_DESKS` table is what makes a lost fixture directory a red test.
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

/// Refuse a release whose predecessor's handoff fixtures are not checked in and pinned,
/// judged on the commit: the ledger, the fixture directories and the guard are read from
/// `commit`'s objects, so `ship check` ([`precheck`]) and `ship release`'s step 4 judge
/// exactly the tree a cut of it ships. Origin's tags are listed through `git`.
///
/// Release N updates to N+1 by handing its sessions over, and only N's fixture directory,
/// run by the guard in [`HANDOFF_FIXTURE_GUARD`], checks N+1's consumer against what N
/// really writes. Without it every handoff test writes and reads with the same build and
/// stays green while the real hop fails.
///
/// The predecessor is [`predecessor_release`] over `RELEASES.ledger`, "shipped" meaning its
/// `v<version>` tag is on origin (minted at publish, deleted by an abandon), so an abandoned
/// and skipped claim (docs/RELEASING.md, "Resume, recut, abandon") is passed over. The
/// version being cut is never its own predecessor, which is all a recut needs. `Ok(None)`
/// only when the ledger records no other version.
pub fn handoff_fixture_gate_at(
    git: &dyn GitRunner,
    commit: &str,
    version: &str,
) -> Result<Option<HandoffFixtures>> {
    handoff_fixture_gate_in(git, &GateTree::Commit { git, commit }, version)
}

/// Where a gate reads the tree it judges: a commit's objects (`ship check`,
/// [`precheck`], and `ship release`'s step 4) — or, for the tests, a scratch tree on
/// disk laid out like one. The handoff-fixture and handoff-policy gates read through
/// it, so the release and the check judge the same bytes by one rule.
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

    /// The directories directly under `dir` that hold a `parent.toml`, sorted;
    /// `None` when `dir` does not exist.
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
                desks.sort();
                Ok(Some(desks))
            }
            GateTree::Commit { git, commit } => {
                // Every file under `dir` at `commit`; a directory git lists
                // nothing under is one the commit does not have.
                let listed = git_ok(
                    *git,
                    &["ls-tree", "-r", "-z", "--name-only", *commit, "--", dir],
                )?
                .stdout_utf8();
                if listed.split('\0').all(str::is_empty) {
                    return Ok(None);
                }
                let prefix = format!("{dir}/");
                let desks: std::collections::BTreeSet<String> = listed
                    .split('\0')
                    .filter_map(|path| path.strip_prefix(prefix.as_str()))
                    .filter_map(|rest| rest.strip_suffix("/parent.toml"))
                    .filter(|desk| !desk.is_empty() && !desk.contains('/'))
                    .map(str::to_string)
                    .collect();
                Ok(Some(desks.into_iter().collect()))
            }
        }
    }
}

/// [`handoff_fixture_gate_at`] over either kind of tree.
fn handoff_fixture_gate_in(
    git: &dyn GitRunner,
    tree: &GateTree<'_>,
    version: &str,
) -> Result<Option<HandoffFixtures>> {
    match predecessor_in(git, tree, version)? {
        Some(release) => handoff_fixtures_of(tree, &release).map(Some),
        None => Ok(None),
    }
}

/// The release a cut of `version` from `tree` succeeds: [`predecessor_release`]
/// over `tree`'s ledger, "shipped" meaning its tag is on origin.
fn predecessor_in(
    git: &dyn GitRunner,
    tree: &GateTree<'_>,
    version: &str,
) -> Result<Option<String>> {
    let ledger_text = tree.text(crate::ledger::LEDGER_FILE)?;
    // One listing, not a probe per version: walking back past an abandoned claim must not
    // cost a network round trip per step.
    let listing = git_ok(git, &["ls-remote", "--tags", "origin"]).map_err(|e| {
        Error::new(format!(
            "handoff gates: cannot list origin's tags, which is how they tell the \
             releases that shipped from claims that were abandoned: {e}"
        ))
    })?;
    let tags = tag_commits(&listing.stdout_utf8());
    let shipped = |v: &str| tags.contains_key(&format!("v{v}"));
    predecessor_release(&ledger_text, version, &shipped)
}

/// The tags in `git ls-remote --tags` output, each with the commit it names:
/// an annotated tag's peeled `^{}` row, else the tag's own row (a lightweight
/// tag).
fn tag_commits(listing: &str) -> std::collections::BTreeMap<String, String> {
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

/// The newest release the ledger records before the one being cut: walk the
/// records newest first, skip `version` itself (a recut re-claims it, so the
/// tail may be the very version being cut) and every version `shipped` says
/// never went out, and answer the first one left.
///
/// `Ok(None)` when the ledger names no other version. When it names some and
/// none of them shipped, that is not "nothing to hand off from" but an origin
/// that carries none of this ledger's releases, so the gate fails closed.
pub fn predecessor_release(
    ledger_text: &str,
    version: &str,
    shipped: &dyn Fn(&str) -> bool,
) -> Result<Option<String>> {
    let mut passed_over: Vec<&str> = Vec::new();
    let records = crate::ledger::parse(ledger_text)?;
    for record in records.iter().rev() {
        let candidate = record.version.as_str();
        if candidate == version || passed_over.contains(&candidate) {
            continue;
        }
        if shipped(candidate) {
            return Ok(Some(candidate.to_string()));
        }
        passed_over.push(candidate);
    }
    match passed_over.first() {
        None => Ok(None),
        Some(newest) => Err(Error::new(format!(
            "handoff-fixture gate: none of the {} earlier versions in {} (newest {newest}) \
             has its v<version> tag on origin, so there is no shipped release to say this \
             cut succeeds — is origin the repository this ledger records?",
            passed_over.len(),
            crate::ledger::LEDGER_FILE,
        ))),
    }
}

/// Judge one release's fixtures: its directory exists, at least one desk in it holds a
/// `parent.toml` (desks the guard can discover), and the guard pins at least one of them
/// (without a pinned row, losing the directory again would leave the guard green).
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
    let pinned: Vec<String> = pinned_desks(&guard)?
        .into_iter()
        .filter(|(row_release, _)| *row_release == tag)
        .map(|(_, desk)| desk)
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
    // One pinned desk is not enough: a release whose generator wrote only `history` would
    // leave most of its producer unchecked. Each required desk must be pinned and on disk
    // for every release from its own floor on, since a desk added for release N can never
    // appear in a frozen release before it.
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

/// The `(release, desk)` rows of the guard's `PINNED_DESKS` table, read from
/// its source. A table this gate cannot find or cannot pair up is an error
/// rather than "no rows": a renamed constant must stop the cut, not pass it.
pub(crate) fn pinned_desks(guard: &str) -> Result<Vec<(String, String)>> {
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
    // The rows' string literals in order, comments dropped: every row is two
    // plain literals, and neither a release nor a desk name holds a quote.
    let literals: Vec<&str> = body
        .lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .flat_map(|line| line.split('"').skip(1).step_by(2))
        .collect();
    if !literals.len().is_multiple_of(2) {
        return Err(unreadable("an odd number of string literals"));
    }
    Ok(literals
        .chunks(2)
        .map(|row| (row[0].to_string(), row[1].to_string()))
        .collect())
}

/// One row of the guard's required desk set: `desk` is required of every
/// release from `from` on.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RequiredDesk {
    /// The first release held to it, as the ledger spells a version
    /// (`0.97.0`, the guard's `v` dropped).
    pub(crate) from: String,
    pub(crate) desk: String,
}

/// The guard's `REQUIRED_DESKS` table — `("vX.Y.Z", "<desk>")` rows, each desk
/// with its own floor — read from its source like [`pinned_desks`] and failing
/// closed like it: a renamed constant, an unpaired row, an empty table, or a
/// floor that is not a version, stops the cut.
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

/// `X.Y.Z` as numbers, for ordering releases (never compared as strings:
/// `0.100.0` is after `0.99.0`).
fn version_triple(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.').map(str::parse::<u64>);
    let triple = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    parts.next().is_none().then_some(triple)
}

/// Whether `release` is `floor` or later. A release the ledger records that
/// is not `X.Y.Z` stops the cut rather than slipping under the floor.
pub(crate) fn release_at_least(release: &str, floor: &str) -> Result<bool> {
    match (version_triple(release), version_triple(floor)) {
        (Some(release), Some(floor)) => Ok(release >= floor),
        _ => Err(Error::new(format!(
            "handoff-fixture gate: cannot order release {release} against the required \
             desk set's floor {floor}"
        ))),
    }
}

/// The release's fixture generator's own target directory in the tree it runs in
/// (`crate::fixtures`): a root sibling the checked-in `.gitignore` names (`/target-*`),
/// so it never dirties the tree the release asserts clean, and apart from `target/`
/// (which may be the `target.noindex` link the release lane validates) and from the
/// release build's products.
pub const HANDOFF_FIXTURE_GUARD_TARGET: &str = "target-fixture-guard";

/// `targo --unverified test -p aterm-gui --lib --target-dir <target> -- <args>` in
/// `tree`, with the build's steering variables (`RUSTFLAGS`, `RUSTC`, …) removed, as
/// for the release build: an inherited `RUSTFLAGS` replaces the config's rustflags
/// and would turn Trust verification on for every unit. The release's own fixture
/// generator runs it (`crate::fixtures`).
pub(crate) fn aterm_gui_lib_test(
    targo: &Path,
    tree: &Path,
    target: &Path,
    args: &[&str],
) -> Command {
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
        .arg("--")
        .args(args)
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

/// Remove `path` (a directory, a file or a link), and nothing when it is absent.
pub(crate) fn clear_path(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// What libtest's output says: the tests that passed and failed over every
/// `test result:` line, and the failing tests by name.
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

/// One refusal shape for every way a predecessor's fixtures can be missing:
/// what is missing and where the steps are.
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

/// `gh auth status` must succeed: the publish half of the cut talks to GitHub, and a dead
/// token found after the build wastes ten minutes. Returns the account name when parseable
/// (transcript only).
pub fn gh_auth() -> Result<Option<String>> {
    let out = command("gh")
        .args(["auth", "status"])
        .output()
        .map_err(|e| {
            Error::new(format!(
                "failed to run `gh auth status` — is the GitHub CLI installed? ({e})"
            ))
        })?;
    if !out.status.success() {
        return Err(Error::new(format!(
            "`gh auth status` failed — run `gh auth login` first: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    // gh versions differ on stdout or stderr; scan both for "… account <name> …".
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let account = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find_map(|w| {
            (w[0] == "account").then(|| {
                w[1].trim_matches(|c: char| !c.is_alphanumeric() && c != '-')
                    .to_string()
            })
        });
    Ok(account)
}

/// trustc's own words for "the off-switch took effect": the gate asks the compiler, not a
/// flag spelling, whether the native lane turned verification off. `targo trust` refuses a
/// project-config `-Ztrust-verify=off`, so the config carries none and `targo --unverified`
/// appends its own. Against [`PROBE_SRC`] the off lane prints `note: compiled WITHOUT
/// Trust verification — 0 obligations checked`; a verifying one prints `1 proved, … out of
/// 1 obligation(s)`.
const OFF_SWITCH_NOTE: &str = "compiled WITHOUT Trust verification";

/// The probe's source. It carries one real Level 0 obligation on purpose (divisor-is-zero
/// on `numerator / denominator`): with none, both lanes print "0 obligations checked" and
/// [`verification_off_verdict`] could not tell them apart.
const PROBE_SRC: &str = "\
pub fn divide(numerator: u32, denominator: u32) -> u32 {
    numerator / denominator
}

fn main() {
    let _ = divide(6, 3);
}
";

/// Did the native lane (`targo --unverified` under the repo's config) turn verification
/// off in the trustc that just ran? Pure over trustc's stderr. The drift that dies here,
/// pre-claim instead of twenty minutes into the build: a `targo` that is not the real
/// targo, or a lane that stopped appending its off-switch, so trustc prints its report
/// instead of [`OFF_SWITCH_NOTE`]. A spelling the compiler cannot parse fails the compile
/// itself, before this call.
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
/// Always on: the repo compiles with Trust, so a missing or broken toolchain is never a
/// fallback. Failures print the exact path.
///
/// Beyond `--version`, it compiles [`PROBE_SRC`] through the native lane itself (`targo
/// --unverified check` in a scratch package with a copy of the repo's .cargo/config.toml):
/// a stage2 whose library build never landed runs but has no std rlibs (every crate
/// E0463s), and an off-switch this trustc does not parse fails every unit the same way.
/// [`verification_off_verdict`] then reads trustc's own note, the only proof the lane's
/// opt-out reached it (the config carries no off-switch, so a direct trustc call would
/// verify).
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
    // Sysroot smoke-compile through the native lane: the stage2's own targo,
    // `--unverified`, under a copy of the repo's config, in a scratch package.
    let targo = resolve_targo()?;
    let dir = std::env::temp_dir().join(format!("aterm-trust-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let setup = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(dir.join("src"))?;
        std::fs::create_dir_all(dir.join(".cargo"))?;
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"aterm-trust-probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
             [workspace]\n",
        )?;
        std::fs::write(dir.join("src/main.rs"), PROBE_SRC)?;
        std::fs::copy(
            repo.join(".cargo/config.toml"),
            dir.join(".cargo/config.toml"),
        )?;
        Ok(())
    })();
    if let Err(e) = setup {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(Error::new(format!(
            "probe package in {}: {e}",
            dir.display()
        )));
    }
    // Scrub inherited compiler steering as the build does (buildplan `build_one`), or a
    // stale RUSTFLAGS would replace the config's flags. Its own CARGO_HOME, as the build
    // pins one, so a user-level `~/.cargo/config.toml` cannot steer the probe; the package
    // has no deps and runs `--offline`, so an empty home serves.
    let out = command(&targo)
        .current_dir(&dir)
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
    let result = match out {
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
    };
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// The x86_64 compat slice builds on upstream stable (buildplan.rs pins
/// `RUSTUP_TOOLCHAIN=stable`), so probe stable's installed targets: a bare `rustup target
/// list` would resolve the repo's `trust` toolchain, which carries no rustup-managed
/// targets. The release is universal and never silently ships a thinner artifact.
/// STOCK EXCEPTION (Trust lacks an x86_64-apple-darwin std: `targo --unverified
/// check --target x86_64-apple-darwin -p aterm` fails E0463).
pub fn x86_target_probe() -> Result<()> {
    if stable_targets_missing(&["x86_64-apple-darwin"])?.is_empty() {
        return Ok(());
    }
    // A fault only: the caller lays out `X86_SLICE_REMEDIES`, since this string is read on
    // two grids (a cut step line and a provision audit line).
    Err(Error::new(
        "x86_64-apple-darwin target missing from the stable toolchain — a universal \
         build is impossible"
            .to_string(),
    ))
}

/// Which of `targets` upstream stable has no std installed for (`rustup +stable target
/// list --installed`): the stock lanes Trust has no std for.
///
/// # Errors
/// rustup cannot be run, or does not answer.
pub fn stable_targets_missing(targets: &[&str]) -> Result<Vec<String>> {
    let out = command("rustup")
        .env("RUSTUP_TOOLCHAIN", "stable")
        .args(["target", "list", "--installed"])
        .output()
        .map_err(|e| {
            Error::new(format!(
                "failed to run rustup ({e}); upstream stable carries the std of the targets \
                 Trust has none for.\n\
                 fix:  install rustup, then `rustup toolchain install stable`"
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

/// The proof, before the release's one build (`artifact::build`), that this builder can
/// build the x86_64 slice (`target`, the stable `x86_64-apple-darwin` std) and run it
/// (`rosetta`): the release is universal and the build runs the slice under Rosetta
/// (docs/DESIGN-intel-just-works-2026-09-14.md §3.1), so a builder that cannot is refused
/// before minutes of compile. Both probes are injected so every arm is testable.
pub(crate) fn universal_gate(
    target: impl FnOnce() -> Result<()>,
    rosetta: impl FnOnce() -> std::result::Result<(), String>,
) -> Result<()> {
    // The probe returns a fault; this caller, the only one that must stop, adds the remedy.
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

/// Whether Rosetta runs a trivial x86_64 program here (`/usr/bin/arch -x86_64
/// /usr/bin/true`); `run` spawns, injected for tests. The one Rosetta probe: the pre-build
/// [`universal_gate`], the provision audit and the post-build slice run all ask it.
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
/// Returns free GiB for the transcript.
pub fn disk_gate(repo: &Path) -> Result<u64> {
    let out = command("df")
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
    // -P guarantees one header line then one line per filesystem; field 4 is
    // "Available" in 1024-byte blocks.
    let avail_kib: u64 = text
        .lines()
        .nth(1)
        .and_then(|l| l.split_whitespace().nth(3))
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
mod universal_gate_tests {
    use std::os::unix::process::ExitStatusExt as _;

    use super::*;

    fn exited(code: i32) -> std::process::Output {
        std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: b"bad CPU type in executable".to_vec(),
        }
    }

    /// A missing stable target stops before Rosetta is asked, with the target remedies
    /// laid out; both present pass.
    #[test]
    fn a_missing_target_stops_before_rosetta_is_asked() {
        universal_gate(
            || Err(Error::new("x86_64-apple-darwin target missing")),
            || unreachable!("a missing target stops before Rosetta is asked"),
        )
        .expect_err("a missing target refuses");
        assert!(universal_gate(|| Ok(()), || Ok(())).is_ok());
    }

    /// The one Rosetta probe runs `/usr/bin/arch -x86_64 /usr/bin/true` (without
    /// `-x86_64` it runs native and passes on any Mac); a failed spawn is the same
    /// refusal as a failed run; only exit 0 passes.
    #[test]
    fn the_rosetta_probe_fails_closed() {
        let mut seen = Vec::new();
        let err = rosetta_runs(&mut |command| {
            seen.push(
                std::iter::once(command.get_program())
                    .chain(command.get_args())
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
            );
            Ok(exited(1))
        })
        .unwrap_err();
        assert_eq!(err, ROSETTA_REFUSAL);
        assert_eq!(
            seen,
            vec![vec!["/usr/bin/arch", "-x86_64", "/usr/bin/true"]]
        );
        assert_eq!(
            rosetta_runs(&mut |_| Err(std::io::Error::other("no arch"))).unwrap_err(),
            ROSETTA_REFUSAL
        );
        assert_eq!(rosetta_runs(&mut |_| Ok(exited(0))), Ok(()));
    }
}

#[cfg(test)]
mod cut_lease_tests {
    use super::*;

    /// A cut leases the toolchain it pinned under the store's prefix (a lease atpkg's gc
    /// and flip gate read, naming the cutter); a toolchain outside the prefix takes none.
    /// Only `run_cut` arms the lease, so tests that resolve the machine's real toolchain
    /// write nothing into its package store.
    #[test]
    fn a_cut_leases_the_toolchain_it_pinned_and_nothing_else_does() {
        assert!(
            !LEASE_ARMED.load(std::sync::atomic::Ordering::Relaxed),
            "no test arms the cut's lease"
        );
        let prefix =
            std::env::temp_dir().join(format!("aterm-release-cut-lease-{}", std::process::id()));
        let _ = fs::remove_dir_all(&prefix);
        let bin = prefix.join("store/trust/9192/bin");
        fs::create_dir_all(&bin).unwrap();
        let lease = take_cut_lease(&prefix, &bin).expect("a store toolchain is leased");
        let subject = atpkg::lease::Subject::build("trust", 9192).unwrap();
        assert_eq!(lease.subject(), &subject);
        let atpkg::lease::Holders::Held(who) = atpkg::lease::holders(&prefix, &subject) else {
            panic!("atpkg reads the cut's lease");
        };
        assert_eq!(who.len(), 1, "{who:?}");
        drop(lease);
        assert!(take_cut_lease(&prefix, &std::env::temp_dir()).is_none());
        let _ = fs::remove_dir_all(&prefix);
    }
}

#[cfg(test)]
mod submodule_tests {
    use super::*;
    use crate::ledger::RunOut;
    use std::sync::Mutex;

    /// A git that answers by argv rather than by position: these tests are about which
    /// question is asked.
    struct FakeGit {
        answers: Mutex<Vec<(&'static str, RunOut)>>,
        calls: Mutex<Vec<String>>,
    }

    fn out(status: i32, stdout: &str) -> RunOut {
        RunOut {
            status,
            stdout: stdout.as_bytes().to_vec(),
            stderr: vec![],
        }
    }

    impl FakeGit {
        fn new(answers: Vec<(&'static str, RunOut)>) -> Self {
            Self {
                answers: Mutex::new(answers),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl GitRunner for FakeGit {
        fn git(&self, args: &[&str]) -> Result<RunOut> {
            let line = args.join(" ");
            self.calls.lock().expect("calls").push(line.clone());
            let mut answers = self.answers.lock().expect("answers");
            // The first matching answer, consumed, so a question asked twice gets its
            // answers in order.
            match answers.iter().position(|(needle, _)| line.contains(needle)) {
                Some(i) => Ok(answers.remove(i).1),
                None => Ok(out(0, "")),
            }
        }
    }

    #[test]
    fn a_submodule_off_its_gitlink_is_refused_and_named() {
        let git = FakeGit::new(vec![(
            "submodule status --recursive",
            out(
                0,
                " 1111111111111111111111111111111111111111 vendor/ok (v0)\n\
                 -2222222222222222222222222222222222222222 vendor/astream\n\
                 +3333333333333333333333333333333333333333 vendor/moved (v1-2-g3333333)\n",
            ),
        )]);
        let err = submodules_at_gitlinks(&git).expect_err("refused");
        let msg = err.to_string();
        assert!(
            msg.contains("-2222") && msg.contains("vendor/astream"),
            "{msg}"
        );
        assert!(
            msg.contains("+3333") && msg.contains("vendor/moved"),
            "{msg}"
        );
        assert!(!msg.contains("vendor/ok"), "{msg}");
    }

    #[test]
    fn a_submodule_status_line_is_off_its_gitlink_unless_it_opens_with_a_space() {
        assert!(submodules_off_gitlink("").is_empty());
        assert!(submodules_off_gitlink(" abc vendor/astream (v0)\n\n").is_empty());
        assert_eq!(
            submodules_off_gitlink("-abc a\n+def b (x)\nUghi c\n jkl d\n"),
            vec!["-abc a", "+def b (x)", "Ughi c"]
        );
    }

    /// `checkout --detach` moves a gitlink and not the submodule checkout, and a linked
    /// worktree starts with none: the alignment updates, then checks.
    #[test]
    fn aligning_updates_every_submodule_and_then_checks_the_result() {
        let git = FakeGit::new(vec![]);
        align_submodules(&git).expect("aligned");
        let calls = git.calls.lock().expect("calls").clone();
        let at = |needle: &str| {
            calls
                .iter()
                .position(|c| c.contains(needle))
                .unwrap_or_else(|| panic!("never asked {needle:?}: {calls:?}"))
        };
        assert!(
            at("submodule update --init --recursive --checkout")
                < at("submodule status --recursive"),
            "the update is checked, not trusted: {calls:?}"
        );
    }

    #[test]
    fn a_submodule_the_cut_tree_cannot_check_out_fails_loudly() {
        let git = FakeGit::new(vec![(
            "submodule update",
            RunOut {
                status: 1,
                stdout: vec![],
                stderr: b"fatal: could not read Username for 'https://github.com'".to_vec(),
            },
        )]);
        align_submodules(&git).expect_err("a stale astream must not be built");
    }

    /// Against real git: a pin the bus's main carries is released; a pin pushed only to a
    /// side branch checks out at its gitlink like any other and is refused, naming the
    /// branch it is not on.
    #[test]
    fn only_a_pin_its_declared_branch_carries_is_released() {
        use crate::ledger::testing::{self, Git};
        let dir = aterm_tempfile::tempdir().unwrap();
        let ident = [
            ("user.name", "Test"),
            ("user.email", "test@example.invalid"),
            ("commit.gpgsign", "false"),
        ];
        let git = |at: &Path, args: &[&str]| {
            let out = testing::git()
                .arg("-C")
                .arg(at)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let bus = dir.path().join("bus");
        testing::init(&bus, &ident);
        git(&bus, &["commit", "-q", "--allow-empty", "-m", "on main"]);
        git(&bus, &["checkout", "-q", "-b", "side"]);
        git(
            &bus,
            &["commit", "-q", "--allow-empty", "-m", "only on side"],
        );
        let side = git(&bus, &["rev-parse", "HEAD"]);
        git(&bus, &["checkout", "-q", "main"]);
        let top = dir.path().join("top");
        testing::init(&top, &ident);
        git(
            &top,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                "-b",
                "main",
                bus.to_str().unwrap(),
                "vendor/bus",
            ],
        );
        git(&top, &["commit", "-q", "-m", "pin the bus's main"]);
        let runner = Git::new(&top);
        submodules_on_declared_branch(&runner).expect("main carries the pin");

        git(&top.join("vendor/bus"), &["checkout", "-q", &side]);
        git(&top, &["commit", "-q", "-am", "pin the side branch"]);
        submodules_on_declared_branch(&runner).expect_err("a side-branch pin is refused");

        // A tag with the declared branch's name is not that branch. In
        // particular, Git's unqualified fetch accepts this tag after the
        // branch is gone, even with `--no-tags`.
        git(&bus, &["tag", "main", &side]);
        git(&bus, &["checkout", "-q", "side"]);
        git(&bus, &["branch", "-D", "main"]);
        submodules_on_declared_branch(&runner)
            .expect_err("a same-named tag cannot stand in for a missing branch");
    }

    /// Unknown is not on: a fetch that fails, or an ancestry git cannot decide,
    /// refuses with git's own why. A submodule that declares no branch is held to
    /// its remote's `HEAD`.
    #[test]
    fn an_unreadable_branch_refuses_and_an_undeclared_one_is_the_remotes_head() {
        let pin = "1".repeat(40);
        let listing = format!(
            "160000 commit {pin}\tvendor/astream\n100644 blob {}\tREADME.md\n",
            "2".repeat(40)
        );
        let fake = |extra: Vec<(&'static str, RunOut)>| {
            let mut answers = vec![
                ("ls-tree -r HEAD", out(0, &listing)),
                (
                    "--get-regexp",
                    out(0, "submodule.vendor/astream.path vendor/astream\n"),
                ),
                ("--get submodule.vendor/astream.branch", out(1, "")),
            ];
            answers.extend(extra);
            FakeGit::new(answers)
        };

        let git = fake(vec![]);
        submodules_on_declared_branch(&git).expect("on the remote's HEAD");
        let calls = git.calls.lock().expect("calls").clone();
        assert!(
            calls.contains(&"-C vendor/astream fetch --quiet --no-tags origin HEAD".to_string()),
            "{calls:?}"
        );
        assert!(
            calls.contains(&format!(
                "-C vendor/astream merge-base --is-ancestor {pin} FETCH_HEAD"
            )),
            "{calls:?}"
        );

        let unfetched = fake(vec![(
            " fetch ",
            RunOut {
                status: 128,
                stdout: vec![],
                stderr: b"fatal: unable to access the remote".to_vec(),
            },
        )]);
        submodules_on_declared_branch(&unfetched).expect_err("no fetch, no verdict");

        let undecided = fake(vec![("merge-base", out(128, ""))]);
        submodules_on_declared_branch(&undecided).expect_err("no ancestry, no verdict");
    }
}

#[cfg(test)]
mod cutter_identity_tests {
    use super::*;
    use crate::ledger::RunOut;

    const HEAD: &str = "2617295d1111111111111111111111111111aaaa";
    const OLDER: &str = "15f2b5b6bc51d9ef56e5e4fa50f434fca07f40ee";

    /// A git that answers `rev-parse <c>^1` with `parent` and the diff with `changed`.
    struct Claimed {
        parent: &'static str,
        changed: &'static str,
    }

    impl GitRunner for Claimed {
        fn git(&self, args: &[&str]) -> Result<RunOut> {
            let text = match args.first() {
                Some(&"rev-parse") => self.parent,
                Some(&"diff") => self.changed,
                _ => "",
            };
            Ok(RunOut {
                status: 0,
                stdout: text.as_bytes().to_vec(),
                stderr: Vec::new(),
            })
        }
    }

    fn unrelated() -> Claimed {
        Claimed {
            parent: OLDER,
            changed: "crates/aterm-release/src/publish.rs\n",
        }
    }

    /// The commit's own binary is its cutter; another commit's binary is refused,
    /// whatever moved between them.
    #[test]
    fn another_commits_binary_is_refused() {
        own_cutter_with(&unrelated(), HEAD, HEAD).expect("its own");
        own_cutter_with(&unrelated(), OLDER, HEAD).expect_err("not its own");
        // a doc-only descendant is another commit too: no source closure is reasoned about
        let docs = Claimed {
            parent: OLDER,
            changed: "docs/NOTE.md\n",
        };
        assert!(own_cutter_with(&docs, OLDER, HEAD).is_err());
    }

    /// The claim commit is the claimed commit plus its ledger line and changelog roll, so
    /// the claimed commit's cutter finishes it (a resume, an abandon, a recovery). Negative
    /// controls: a third path, or another parent, is another commit.
    #[test]
    fn a_claim_commit_is_finished_by_the_cutter_of_the_commit_it_claims() {
        let claim = Claimed {
            parent: OLDER,
            changed: "CHANGELOG.md\nRELEASES.ledger\n",
        };
        own_cutter_with(&claim, OLDER, HEAD).expect("the claim's cutter");
        let recut = Claimed {
            parent: OLDER,
            changed: "RELEASES.ledger\n",
        };
        assert!(
            own_cutter_with(&recut, OLDER, HEAD).is_ok(),
            "a recut rolls no section"
        );
        let more = Claimed {
            parent: OLDER,
            changed: "CHANGELOG.md\nRELEASES.ledger\ncrates/aterm-release/src/gates.rs\n",
        };
        assert!(own_cutter_with(&more, OLDER, HEAD).is_err());
        let other_parent = Claimed {
            parent: "3333333333333333333333333333333333333333",
            changed: "RELEASES.ledger\n",
        };
        assert!(own_cutter_with(&other_parent, OLDER, HEAD).is_err());
    }

    /// "Cannot tell" refuses: a binary that is no commit's build is never any commit's
    /// cutter, not even through a claim commit.
    #[test]
    fn a_binary_that_is_no_commits_build_fails_closed() {
        let claim = Claimed {
            parent: "unknown",
            changed: "RELEASES.ledger\n",
        };
        let none = crate::cutter::NotOwn {
            why: "its checkout is dirty".into(),
            fix: "commit it".into(),
        };
        let not = own_cutter_of(&claim, Err(&none), HEAD).expect_err("none");
        assert!(not.why.ends_with(&none.why), "{not:?}");
        assert_eq!(not.fix, none.fix, "the verdict's own remedy");
        own_cutter_with(&claim, "unknown", HEAD).expect_err("not an id");
    }

    /// A dry-run publishes nowhere, so it proceeds — but the gate says so; a real run
    /// is refused.
    #[test]
    fn only_a_dry_run_proceeds_with_another_commits_cutter() {
        let other = "4444444444444444444444444444444444444444";
        assert!(own_cutter_gate(&unrelated(), other, true).is_ok());
        own_cutter_gate(&unrelated(), other, false).expect_err("refused");
    }
}

#[cfg(test)]
mod toolchain_pin_tests {
    //! [`trust_stage2_bin`]'s pin: one compiler for the whole process.

    use super::*;
    use std::sync::OnceLock;

    #[test]
    fn the_first_resolution_is_the_one_the_whole_process_gets() {
        let cell: OnceLock<PathBuf> = OnceLock::new();
        let first = pin_first_success(&cell, || Ok(PathBuf::from("/seal/alpha/bin")))
            .expect("the first resolution succeeds");
        assert_eq!(first, Path::new("/seal/alpha/bin"));

        // A seal lands mid-run and the store's `current` now points elsewhere.
        // The running cut must not notice.
        let second = pin_first_success(&cell, || {
            panic!("a pinned toolchain must never be re-resolved")
        })
        .expect("the pin answers without resolving");
        assert_eq!(second, first);
    }

    #[test]
    fn a_failed_resolution_pins_nothing_and_is_retried() {
        let cell: OnceLock<PathBuf> = OnceLock::new();
        assert!(pin_first_success(&cell, || Err(Error::new("no Trust toolchain found"))).is_err());
        assert!(
            cell.get().is_none(),
            "a failure has no toolchain identity to protect; caching it would outlive \
             the install the error text tells the operator to perform"
        );
        let after = pin_first_success(&cell, || Ok(PathBuf::from("/seal/beta/bin")))
            .expect("the retry resolves");
        assert_eq!(after, Path::new("/seal/beta/bin"));
    }

    /// `ship release`'s prover, pinned before any resolution: every later ask of the
    /// process answers it, never the store's current build — and a second, different
    /// pin is refused rather than splitting the run across two compilers.
    #[test]
    fn a_release_pins_its_journaled_prover_before_any_resolution() {
        let cell: OnceLock<PathBuf> = OnceLock::new();
        let journaled = Path::new("/store/trust/9192/bin");
        pin_into(&cell, journaled).expect("first pin");
        let asked = pin_first_success(&cell, || Ok(PathBuf::from("/store/trust/current/bin")))
            .expect("pinned");
        assert_eq!(asked, journaled, "the store's current build is never asked");
        assert!(
            pin_into(&cell, journaled).is_ok(),
            "the same pin again is no change"
        );
        assert!(pin_into(&cell, Path::new("/store/trust/9300/bin")).is_err());
    }
}

#[cfg(test)]
mod native_lane_probe_tests {
    //! The trustc probe judges the compiler's own words, never a flag spelling.

    use super::*;

    #[test]
    fn the_verdict_reads_the_compilers_note() {
        assert!(
            verification_off_verdict(
                "note: compiled WITHOUT Trust verification — 0 obligations checked\n"
            )
            .is_ok(),
            "the compiler is the authority on its own flags"
        );
    }

    /// The value drift: the switch parses, the compiler verifies anyway. This is
    /// the measured verifying output for [`PROBE_SRC`].
    #[test]
    fn a_switch_that_parsed_but_did_not_switch_is_refused() {
        let report = "=== Trust Verification Report (probe) ===\n\
                      note: Trust verification: 1 proved, 0 failed, 0 unknown, 0 timed out, \
                      0 runtime-checked out of 1 obligation(s)\n";
        verification_off_verdict(report).expect_err("a compiler that verified is not an off lane");
    }
}

#[cfg(test)]
mod tag_free_tests {
    use super::*;
    use crate::ledger::RunOut;

    /// A checkout whose tag `v…` is (or is not) here and on origin.
    struct Tags {
        local: bool,
        on_origin: bool,
    }

    impl GitRunner for Tags {
        fn git(&self, args: &[&str]) -> Result<RunOut> {
            let (status, stdout) = match args.first().copied() {
                Some("rev-parse") => (i32::from(!self.local), String::new()),
                Some("ls-remote") if self.on_origin => {
                    (0, format!("{:040x}\t{}\n", 1, args[args.len() - 1]))
                }
                Some("ls-remote") => (0, String::new()),
                _ => panic!("tag_free asks only rev-parse and ls-remote, not {args:?}"),
            };
            Ok(RunOut {
                status,
                stdout: stdout.into_bytes(),
                stderr: vec![],
            })
        }
    }

    /// A tag on origin, or a leftover local one, refuses the cut; a free tag passes.
    #[test]
    fn a_taken_tag_refuses_the_cut() {
        for (local, on_origin) in [(true, true), (true, false)] {
            tag_free(&Tags { local, on_origin }, "0.97.0").expect_err("a taken tag is refused");
        }
        let free = Tags {
            local: false,
            on_origin: false,
        };
        tag_free(&free, "0.97.0").expect("a free tag passes");
    }
}

#[cfg(test)]
mod handoff_fixture_gate_tests {
    use super::*;
    use crate::ledger::RunOut;
    use std::sync::Mutex;

    /// A scratch repository laid out like the real one: the ledger at the root,
    /// the guard's source and the fixture tree at their real relative paths.
    /// Removed on drop, pass or fail.
    struct Scratch(aterm_tempfile::TempDir);

    impl Scratch {
        fn new(label: &str) -> Self {
            Scratch(
                aterm_tempfile::Builder::new()
                    .prefix(&format!("aterm-release-handoff-{label}-"))
                    .tempdir()
                    .expect("scratch repo"),
            )
        }
        fn write(&self, rel: &str, text: &str) {
            let path = self.0.path().join(rel);
            fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
            fs::write(path, text).expect("write");
        }
        fn ledger(&self, versions: &[&str]) {
            let mut text = String::from("# aterm release ledger\n");
            for (i, v) in versions.iter().enumerate() {
                text.push_str(&format!("{} {v}\n", 1_790_000_000 + i));
            }
            self.write(crate::ledger::LEDGER_FILE, &text);
        }
        /// The guard's source, carrying `rows` in the real file's shape, and a
        /// required desk set that starts at v0.97.0 (so it binds none of the
        /// older releases these tests cut after).
        fn pins(&self, rows: &[(&str, &str)]) {
            self.pins_requiring(rows, "v0.97.0", &["history", "twelve-panes"]);
        }
        /// [`Self::pins`] with the required desk set spelled out, every desk
        /// required from `from` on.
        fn pins_requiring(&self, rows: &[(&str, &str)], from: &str, required: &[&str]) {
            let required: Vec<(&str, &str)> = required.iter().map(|desk| (from, *desk)).collect();
            self.pins_required_rows(rows, &required);
        }
        /// [`Self::pins`] with the required rows spelled out, each with its own
        /// floor.
        fn pins_required_rows(&self, rows: &[(&str, &str)], required: &[(&str, &str)]) {
            let mut text = String::from(
                "/// Every `(release, desk)` checked in — \"pinned\".\n\
                 const PINNED_DESKS: &[(&str, &str)] = &[\n",
            );
            for (release, desk) in rows {
                text.push_str(&format!("    (\"{release}\", \"{desk}\"),\n"));
            }
            text.push_str("];\n\nconst REQUIRED_DESKS: &[(&str, &str)] = &[\n");
            for (from, desk) in required {
                text.push_str(&format!("    (\"{from}\", \"{desk}\"),\n"));
            }
            text.push_str("];\n\nconst OTHER: &[&str] = &[\"not\", \"a\", \"row\"];\n");
            self.write(HANDOFF_FIXTURE_GUARD, &text);
        }
        fn desk(&self, release: &str, desk: &str) {
            self.write(
                &format!("{HANDOFF_FIXTURE_ROOT}/v{release}/{desk}/parent.toml"),
                "producer_version = \"x\"\n",
            );
        }
    }

    /// Origin, as far as this gate asks it anything: its tag listing, in
    /// `git ls-remote --tags` shape (annotated tags carry a peeled `^{}` row).
    struct Origin {
        tags: Vec<&'static str>,
        asked: Mutex<Vec<String>>,
    }

    impl Origin {
        fn with(tags: &[&'static str]) -> Self {
            Origin {
                tags: tags.to_vec(),
                asked: Mutex::new(Vec::new()),
            }
        }
    }

    impl GitRunner for Origin {
        fn git(&self, args: &[&str]) -> Result<RunOut> {
            let line = args.join(" ");
            self.asked.lock().expect("asked").push(line.clone());
            assert_eq!(
                line, "ls-remote --tags origin",
                "the gate asks one question"
            );
            let mut stdout = String::new();
            for tag in &self.tags {
                stdout.push_str(&format!("{:040x}\trefs/tags/{tag}\n", 1));
                stdout.push_str(&format!("{:040x}\trefs/tags/{tag}^{{}}\n", 2));
            }
            Ok(RunOut {
                status: 0,
                stdout: stdout.into_bytes(),
                stderr: vec![],
            })
        }
    }

    /// Every ledger version shipped: the rule under test is then the ledger walk.
    fn all_shipped(_: &str) -> bool {
        true
    }

    /// [`handoff_fixture_gate_at`] over a scratch tree on disk laid out like a commit.
    fn handoff_fixture_gate(
        git: &dyn GitRunner,
        repo: &Path,
        version: &str,
    ) -> Result<Option<HandoffFixtures>> {
        handoff_fixture_gate_in(git, &GateTree::Dir(repo), version)
    }

    fn refusal(scratch: &Scratch, origin: &Origin, version: &str) {
        handoff_fixture_gate(origin, scratch.0.path(), version)
            .expect_err("the cut must be refused before the claim");
    }

    /// The case this gate exists for: v0.92.0 shipped and its directory was
    /// never added.
    #[test]
    fn a_missing_release_directory_is_refused_naming_it() {
        let scratch = Scratch::new("missing");
        scratch.ledger(&["0.91.0", "0.92.0"]);
        scratch.desk("0.91.0", "history");
        scratch.pins(&[("v0.91.0", "history"), ("v0.92.0", "history")]);
        let origin = Origin::with(&["v0.91.0", "v0.92.0"]);
        refusal(&scratch, &origin, "0.93.0");
    }

    /// A directory is not fixtures: without a `parent.toml` the guard has no
    /// desk to run, so an empty or half-copied directory is refused too.
    #[test]
    fn a_directory_without_a_parent_toml_is_refused() {
        let scratch = Scratch::new("no-parent");
        scratch.ledger(&["0.91.0", "0.92.0"]);
        scratch.write(
            &format!("{HANDOFF_FIXTURE_ROOT}/v0.92.0/history/s0.meta.json"),
            "{}",
        );
        scratch.write(&format!("{HANDOFF_FIXTURE_ROOT}/v0.92.0/notes.txt"), "");
        scratch.pins(&[("v0.92.0", "history")]);
        let origin = Origin::with(&["v0.91.0", "v0.92.0"]);
        refusal(&scratch, &origin, "0.93.0");
    }

    /// Desks on disk that the guard does not pin are the half-done step: the
    /// guard runs them today, and nothing notices if they are lost tomorrow.
    #[test]
    fn a_release_the_guard_does_not_pin_is_refused() {
        let scratch = Scratch::new("unpinned");
        scratch.ledger(&["0.91.0", "0.92.0"]);
        scratch.desk("0.92.0", "history");
        scratch.desk("0.92.0", "twelve-panes");
        scratch.pins(&[("v0.91.0", "history")]);
        let origin = Origin::with(&["v0.91.0", "v0.92.0"]);
        refusal(&scratch, &origin, "0.93.0");
    }

    #[test]
    fn checked_in_and_pinned_passes() {
        let scratch = Scratch::new("present");
        scratch.ledger(&["0.91.0", "0.92.0"]);
        scratch.desk("0.92.0", "twelve-panes");
        scratch.desk("0.92.0", "history");
        scratch.pins(&[
            ("v0.91.0", "history"),
            ("v0.92.0", "history"),
            ("v0.92.0", "twelve-panes"),
        ]);
        let origin = Origin::with(&["v0.91.0", "v0.92.0"]);
        let found = handoff_fixture_gate(&origin, scratch.0.path(), "0.93.0")
            .expect("the predecessor's fixtures are checked in and pinned");
        assert_eq!(
            found,
            Some(HandoffFixtures {
                release: "0.92.0".to_string(),
                desks: vec!["history".to_string(), "twelve-panes".to_string()],
                pinned: vec!["history".to_string(), "twelve-panes".to_string()],
            })
        );
        assert_eq!(origin.asked.lock().expect("asked").len(), 1, "one listing");
    }

    /// A recut re-claims the version it cuts, so the ledger's tail — once or
    /// several times — is that version. Its predecessor is the release before,
    /// whatever the tags say: a version is never its own predecessor.
    #[test]
    fn a_recut_judges_the_release_before_the_version_it_recuts() {
        let once = "1 0.91.0\n2 0.92.0\n3 0.93.0\n";
        let twice = "1 0.91.0\n2 0.92.0\n3 0.93.0\n4 0.92.0\n5 0.93.0\n";
        for ledger in [once, twice] {
            assert_eq!(
                predecessor_release(ledger, "0.93.0", &all_shipped).expect("walks"),
                Some("0.92.0".to_string()),
                "{ledger}"
            );
        }
        // A fresh cut after the same ledger succeeds the tail itself.
        assert_eq!(
            predecessor_release(once, "0.94.0", &all_shipped).expect("walks"),
            Some("0.93.0".to_string())
        );
        // And through the gate, on a recut whose tail has no fixtures of its own.
        let scratch = Scratch::new("recut");
        scratch.ledger(&["0.91.0", "0.92.0", "0.93.0", "0.93.0"]);
        scratch.desk("0.92.0", "history");
        scratch.pins(&[("v0.92.0", "history")]);
        let origin = Origin::with(&["v0.91.0", "v0.92.0"]);
        let found = handoff_fixture_gate(&origin, scratch.0.path(), "0.93.0")
            .expect("a recut of 0.93.0 succeeds 0.92.0")
            .expect("a predecessor");
        assert_eq!(found.release, "0.92.0");
    }

    /// docs/RELEASING.md's abandon-and-skip: 0.93.0 was claimed, abandoned (its
    /// tag deleted) and the operator moved on to 0.94.0. 0.93.0 never shipped,
    /// and no worktree at its tag can exist, so the predecessor is 0.92.0.
    #[test]
    fn a_claim_abandoned_and_skipped_is_passed_over() {
        let scratch = Scratch::new("abandoned");
        scratch.ledger(&["0.91.0", "0.92.0", "0.93.0"]);
        scratch.desk("0.92.0", "history");
        scratch.pins(&[("v0.92.0", "history")]);
        let origin = Origin::with(&["v0.91.0", "v0.92.0"]);
        let found = handoff_fixture_gate(&origin, scratch.0.path(), "0.94.0")
            .expect("the abandoned claim is not a release")
            .expect("a predecessor");
        assert_eq!(found.release, "0.92.0");
    }

    /// Tags answer "did it ship", so an origin carrying none of the ledger's
    /// releases cannot be read as "nothing to hand off from".
    #[test]
    fn an_origin_carrying_none_of_the_ledger_fails_closed() {
        let scratch = Scratch::new("foreign-origin");
        scratch.ledger(&["0.91.0", "0.92.0"]);
        scratch.desk("0.92.0", "history");
        scratch.pins(&[("v0.92.0", "history")]);
        let origin = Origin::with(&["v9.9.9"]);
        refusal(&scratch, &origin, "0.93.0");
    }

    /// The only `None`: a ledger that records nothing but this version.
    #[test]
    fn a_ledger_with_no_other_version_has_no_predecessor() {
        assert_eq!(
            predecessor_release("# seed\n1 0.1.0\n", "0.1.0", &all_shipped).expect("walks"),
            None
        );
    }

    /// The table is read from the guard's source, so what the parser accepts is
    /// part of the gate: comments are not rows, other tables are not rows, and
    /// a table it cannot find or pair up stops the cut.
    #[test]
    fn the_pinned_table_is_read_from_source_and_fails_closed() {
        let source = "const PINNED_DESKS: &[(&str, &str)] = &[\n\
                      \x20   // (\"v0.1.0\", \"commented-out\"),\n\
                      \x20   (\"v0.91.0\", \"history\"), // \"trailing\"\n\
                      \x20   (\n\
                      \x20       \"v0.92.0\",\n\
                      \x20       \"twelve-panes\",\n\
                      \x20   ),\n\
                      ];\n\
                      const NEXT: &[&str] = &[\"x\"];\n";
        assert_eq!(
            pinned_desks(source).expect("parses"),
            vec![
                ("v0.91.0".to_string(), "history".to_string()),
                ("v0.92.0".to_string(), "twelve-panes".to_string()),
            ]
        );
        for broken in [
            "const PINNED: &[(&str, &str)] = &[(\"v0.92.0\", \"history\")];",
            "const PINNED_DESKS: &[(&str, &str)] = &[(\"v0.92.0\", \"history\")",
            "const PINNED_DESKS: &[(&str, &str)] = &[(\"v0.92.0\")];",
        ] {
            pinned_desks(broken).expect_err("an unreadable table stops the cut");
        }
    }

    /// From the guard's floor on, a predecessor missing a required desk (not on disk, or on
    /// disk and not pinned) refuses the cut, naming it.
    #[test]
    fn a_predecessor_missing_a_required_desk_is_refused() {
        let required = ["history", "stalled-sequence", "twelve-panes"];
        let scratch = Scratch::new("required-missing");
        scratch.ledger(&["0.95.0", "0.97.0"]);
        scratch.desk("0.97.0", "history");
        scratch.desk("0.97.0", "twelve-panes");
        scratch.desk("0.97.0", "claude-code-1049");
        scratch.pins_requiring(
            &[
                ("v0.97.0", "claude-code-1049"),
                ("v0.97.0", "history"),
                ("v0.97.0", "twelve-panes"),
            ],
            "v0.97.0",
            &required,
        );
        let origin = Origin::with(&["v0.95.0", "v0.97.0"]);
        refusal(&scratch, &origin, "0.98.0");
        // On disk and not pinned is missing too: the guard would not notice it
        // lost.
        scratch.desk("0.97.0", "stalled-sequence");
        refusal(&scratch, &origin, "0.98.0");
        // And pinned and on disk passes.
        scratch.pins_requiring(
            &[
                ("v0.97.0", "claude-code-1049"),
                ("v0.97.0", "history"),
                ("v0.97.0", "stalled-sequence"),
                ("v0.97.0", "twelve-panes"),
            ],
            "v0.97.0",
            &required,
        );
        let found = handoff_fixture_gate(&origin, scratch.0.path(), "0.98.0")
            .expect("the whole set is checked in and pinned")
            .expect("a predecessor");
        assert_eq!(found.release, "0.97.0");
    }

    /// Releases before the floor are frozen with the desks their generators
    /// could make — v0.95.0 has no `stalled-sequence` — and are not held to
    /// the set.
    #[test]
    fn a_release_before_the_floor_is_not_held_to_the_required_set() {
        let scratch = Scratch::new("required-floor");
        scratch.ledger(&["0.94.0", "0.95.0"]);
        scratch.desk("0.95.0", "history");
        scratch.pins_requiring(
            &[("v0.95.0", "history")],
            "v0.97.0",
            &["history", "stalled-sequence"],
        );
        let origin = Origin::with(&["v0.94.0", "v0.95.0"]);
        let found = handoff_fixture_gate(&origin, scratch.0.path(), "0.97.0")
            .expect("v0.95.0 predates the floor")
            .expect("a predecessor");
        assert_eq!(found.release, "0.95.0");
        // Ordered as numbers, not strings: 0.100.0 is past a 0.97.0 floor.
        assert!(release_at_least("0.100.0", "0.97.0").expect("orders"));
        assert!(!release_at_least("0.9.0", "0.97.0").expect("orders"));
        assert!(release_at_least("0.97", "0.97.0").is_err(), "not X.Y.Z");
    }

    /// With `split-sequences` required from v0.98.0 on, cutting after v0.97.0 (whose frozen
    /// fixtures can never gain it) passes on its own desks, and cutting after v0.98.0 asks
    /// for it.
    #[test]
    fn a_desk_added_for_a_later_release_asks_nothing_of_an_earlier_one() {
        let scratch = Scratch::new("required-per-desk");
        scratch.ledger(&["0.97.0", "0.98.0"]);
        scratch.desk("0.97.0", "history");
        scratch.desk("0.98.0", "history");
        let required = [("v0.97.0", "history"), ("v0.98.0", "split-sequences")];
        scratch.pins_required_rows(&[("v0.97.0", "history"), ("v0.98.0", "history")], &required);
        let origin = Origin::with(&["v0.97.0"]);
        let found = handoff_fixture_gate(&origin, scratch.0.path(), "0.98.0")
            .expect("v0.97.0 is not asked for a desk it can never have")
            .expect("a predecessor");
        assert_eq!(found.release, "0.97.0");
        // The release the desk was added for is held to it.
        let origin = Origin::with(&["v0.97.0", "v0.98.0"]);
        refusal(&scratch, &origin, "0.99.0");
        scratch.desk("0.98.0", "split-sequences");
        scratch.pins_required_rows(
            &[
                ("v0.97.0", "history"),
                ("v0.98.0", "history"),
                ("v0.98.0", "split-sequences"),
            ],
            &required,
        );
        let found = handoff_fixture_gate(&origin, scratch.0.path(), "0.99.0")
            .expect("the whole set from each floor")
            .expect("a predecessor");
        assert_eq!(found.release, "0.98.0");
    }

    /// The required set is read from the guard's source, and fails closed like
    /// the pinned table: a table or floor it cannot find stops the cut.
    #[test]
    fn the_required_table_is_read_from_source_and_fails_closed() {
        let source = "const REQUIRED_DESKS: &[(&str, &str)] = &[\n\
                      \x20   (\"v0.97.0\", \"history\"),\n\
                      \x20   // (\"v0.97.0\", \"commented-out\"),\n\
                      \x20   (\"v0.98.0\", \"split-sequences\"), // trailing\n\
                      ];\n";
        assert_eq!(
            required_desks(source).expect("parses"),
            [
                RequiredDesk {
                    from: "0.97.0".to_string(),
                    desk: "history".to_string(),
                },
                RequiredDesk {
                    from: "0.98.0".to_string(),
                    desk: "split-sequences".to_string(),
                },
            ]
        );
        for broken in [
            "const REQUIRED: &[(&str, &str)] = &[(\"v0.97.0\", \"history\")];",
            "const REQUIRED_DESKS: &[(&str, &str)] = &[];",
            // A desk with no floor.
            "const REQUIRED_DESKS: &[&str] = &[\"history\"];",
            "const REQUIRED_DESKS: &[(&str, &str)] = &[(\"0.97\", \"history\")];",
            "const REQUIRED_DESKS: &[(&str, &str)] = &[(\"history\", \"v0.97.0\")];",
        ] {
            required_desks(broken).expect_err("an unreadable set stops the cut");
        }
    }

    #[test]
    fn tag_listing_folds_peeled_rows_and_ignores_other_refs() {
        let listing = "aaa\trefs/tags/v0.92.0\nbbb\trefs/tags/v0.92.0^{}\n\
                       ccc\trefs/tags/public/v0.92.0\nddd\trefs/heads/main\n";
        let tags = tag_commits(listing);
        assert_eq!(tags["v0.92.0"], "bbb", "the commit an annotated tag names");
        assert_eq!(tags["public/v0.92.0"], "ccc", "kept, but never `v…`");
        assert_eq!(tags.len(), 2, "{tags:?}");
    }
}

#[cfg(test)]
mod handoff_policy_gate_tests {
    use super::*;
    use aterm_update_core::handoff_policy::SOURCE_PATH;

    /// [`handoff_policy_gate_at`] over a scratch tree on disk: the bundle step's strict
    /// read of the file, then what it must keep.
    fn handoff_policy_gate(
        git: &dyn GitRunner,
        repo: &Path,
        predecessor: Option<&str>,
    ) -> Result<CutHandoffPolicy> {
        let (text, policy) = crate::bundle::handoff_policy_for_cut(repo)
            .map_err(|why| Error::new(format!("handoff-policy gate: {why}")))?;
        sealed_policy_kept(git, &GateTree::Dir(repo), predecessor, policy, &text)
    }

    /// A cut with no predecessor asks git nothing; any call is a test failure.
    struct NoGit;

    impl GitRunner for NoGit {
        fn git(&self, args: &[&str]) -> Result<crate::ledger::RunOut> {
            panic!("the policy gate ran git with no predecessor: {args:?}")
        }
    }

    /// A scratch repository holding only `publish/handoff-policy.toml`, removed
    /// on drop.
    struct Scratch(aterm_tempfile::TempDir);

    impl Scratch {
        fn with_policy(label: &str, text: Option<&str>) -> Self {
            let dir = aterm_tempfile::Builder::new()
                .prefix(&format!("aterm-release-policy-{label}-"))
                .tempdir()
                .expect("scratch repo");
            fs::create_dir_all(dir.path().join("publish")).expect("scratch repo");
            if let Some(text) = text {
                fs::write(dir.path().join(SOURCE_PATH), text).expect("write the policy");
            }
            Scratch(dir)
        }
    }

    /// The checked-in policy passes the strict read, so an edit it refuses fails here, in
    /// the commit that made it. What it must keep is judged by `ship check` and the cut.
    #[test]
    fn the_checked_in_policy_passes_the_gate() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/aterm-release sits two levels under the root");
        crate::bundle::handoff_policy_for_cut(root)
            .expect("the checked-in policy is one producers can follow");
    }

    /// Everything a producer would silently IGNORE refuses the cut instead, by
    /// name, before a build number is claimed: a missing file, a typo'd key,
    /// the reserved `seamless`, a value v1 does not define, a missing schema.
    #[test]
    fn a_policy_producers_could_not_follow_refuses_the_cut() {
        for (label, text, why) in [
            ("missing", None, "handoff-policy.toml"),
            (
                "typo",
                Some("schema = 1\ncary = \"repaint\"\n"),
                "unknown key `cary`",
            ),
            (
                "reserved",
                Some("schema = 1\nseamless = false\n"),
                "`seamless` is reserved",
            ),
            ("value", Some("schema = 1\ncarry = \"blank\"\n"), "`carry`"),
            ("schema", Some("carry = \"repaint\"\n"), "schema"),
            (
                "range",
                Some("schema = 1\napplies_to_producers = [9, 2]\n"),
                "empty range",
            ),
            (
                "unranged",
                Some("schema = 1\ncarry = \"repaint\"\n"),
                "a knob outside `[[ranges]]`",
            ),
            (
                "the one-cut retire",
                Some("schema = 1\nretire = { producers = [1, 2], reason = \"gone\" }\n"),
                "`retire` is now a `[[retired]]` table",
            ),
            (
                "a rescue with no reason",
                Some(
                    "schema = 1\napplies_to_producers = [1, 2]\ncarry = \"repaint\"\n\n\
                     [[ranges]]\nproducers = [1, 2]\ncarry = \"repaint\"\n",
                ),
                "no `reason`",
            ),
            (
                "not the hull",
                Some(
                    "schema = 1\napplies_to_producers = [1, 2]\ncarry = \"visible\"\n\n\
                     [[ranges]]\nproducers = [1, 2]\ncarry = \"repaint\"\nreason = \"x\"\n",
                ),
                "not the hull of `[[ranges]]`",
            ),
            (
                "a knob with nothing before it",
                Some(
                    "schema = 1\napplies_to_producers = [1790120000, 1790129999]\ncarry = \
                     \"repaint\"\n\n[[ranges]]\nproducers = [1790120000, 1790129999]\ncarry = \
                     \"repaint\"\nreason = \"x\"\n",
                ),
                "no build can hand over",
            ),
        ] {
            let scratch = Scratch::with_policy(label, text);
            let refused = handoff_policy_gate(&NoGit, scratch.0.path(), None)
                .err()
                .unwrap_or_else(|| panic!("{label}: the gate passed"))
                .to_string();
            assert!(
                refused.contains("handoff-policy gate") && refused.contains(why),
                "{label}: {refused}"
            );
        }
    }
}

#[cfg(test)]
mod staged_bundle_liveness_tests {
    //! The pre-claim liveness gate: a live process under a cut staging bundle refuses,
    //! "could not look" refuses, and only an answered empty look passes.

    use super::*;
    use crate::bundle::RunningProcess;

    const DIST: &str = "/Users//a/aterm/dist";

    fn procs(rows: &[(&str, &str)]) -> Vec<RunningProcess> {
        rows.iter()
            .map(|(pid, comm)| ((*pid).to_string(), (*comm).to_string()))
            .collect()
    }

    #[test]
    fn a_live_staging_bundle_refuses_naming_the_pid_and_the_path() {
        let dist = Path::new(DIST);
        let listed = procs(&[
            (
                "7",
                "/Users//a/aterm/dist/cut-1790000000.noindex/aterm.app/Contents/MacOS/aterm",
            ),
            ("4242", "/Applications/aterm.app/Contents/MacOS/aterm"),
        ]);
        let error = staged_bundle_liveness_gate(dist, &|| Some(listed.clone()))
            .expect_err("a live per-claim staging bundle must refuse the cut")
            .to_string();
        assert!(error.contains("pid 7 "), "{error}");
        assert!(
            !error.contains("4242"),
            "an installed copy is not a staging bundle: {error}"
        );
    }

    #[test]
    fn an_unknown_process_list_refuses() {
        staged_bundle_liveness_gate(Path::new(DIST), &|| None)
            .expect_err("\"could not look\" never licenses a delete");
    }

    #[test]
    fn an_answered_look_with_nothing_under_a_staging_dir_passes() {
        let dist = Path::new(DIST);
        assert_eq!(
            staged_bundle_liveness_gate(dist, &|| Some(Vec::new())).unwrap(),
            0
        );
        // Negative control for the matcher: what is not a staging bundle passes
        // (`dist/aterm.app`, its rollback sibling, look-alikes, an install elsewhere, and
        // the fixed `dist/cut-app/`, which no cut deletes).
        let listed = procs(&[
            (
                "85619",
                "/Users//a/aterm/dist/cut-app/aterm.app/Contents/MacOS/aterm",
            ),
            ("1", "/Users//a/aterm/dist/aterm.app/Contents/MacOS/aterm"),
            (
                "2",
                "/Users//a/aterm/dist/aterm.app.rollback/Contents/MacOS/aterm",
            ),
            (
                "3",
                "/Users//a/aterm/dist/cut-app-old/aterm.app/Contents/MacOS/aterm",
            ),
            (
                "4",
                "/Users//a/aterm/dist/cut-12x.noindex/aterm.app/Contents/MacOS/aterm",
            ),
            (
                "5",
                "/Users//a/aterm/dist-dev/cut-app/aterm.app/Contents/MacOS/aterm",
            ),
            ("6", "/Applications/aterm.app/Contents/MacOS/aterm"),
        ]);
        assert_eq!(
            staged_bundle_liveness_gate(dist, &|| Some(listed.clone())).unwrap(),
            7
        );
    }
}

#[cfg(test)]
mod published_commit_tests {
    //! The cut builds the published commit, against real git: the cut tree goes to the
    //! commit `pub publish` recorded whatever main's tip is, and the operator's checkout
    //! never moves.

    use super::*;
    use crate::ledger::testing::{self, Git};

    struct Repo {
        root: aterm_tempfile::TempDir,
    }

    impl Repo {
        fn new(label: &str) -> Self {
            let root = aterm_tempfile::Builder::new()
                .prefix(&format!("aterm-release-published-{label}-"))
                .tempdir()
                .unwrap();
            let repo = Self { root };
            testing::init(
                &repo.work(),
                &[
                    ("user.name", "Test"),
                    ("user.email", "test@example.invalid"),
                    ("commit.gpgsign", "false"),
                ],
            );
            repo
        }
        fn work(&self) -> PathBuf {
            self.root.path().join("work")
        }
        fn run(&self, args: &[&str]) -> String {
            let out = testing::git()
                .arg("-C")
                .arg(self.work())
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        /// Writes `files`, stages them with one `git add`, and commits.
        fn commit(&self, subject: &str, files: &[(&str, &str)]) -> String {
            for (path, body) in files {
                let full = self.work().join(path);
                fs::create_dir_all(full.parent().unwrap()).unwrap();
                fs::write(full, body).unwrap();
            }
            if !files.is_empty() {
                let mut add = vec!["add", "--"];
                add.extend(files.iter().map(|(path, _)| *path));
                self.run(&add);
            }
            self.run(&["commit", "-q", "--allow-empty", "-m", subject]);
            self.run(&["rev-parse", "HEAD"])
        }
        /// A bare `origin` holding main as it stands now.
        fn publish_origin(&self) {
            let origin = self.root.path().join("origin.git");
            let out = testing::git()
                .args(["clone", "-q", "--bare"])
                .arg(self.work())
                .arg(&origin)
                .output()
                .unwrap();
            assert!(out.status.success());
            testing::config(&origin, &[]);
            self.run(&["remote", "add", "origin", origin.to_str().unwrap()]);
            self.run(&["fetch", "-q", "origin"]);
        }
        fn push(&self) {
            self.run(&["push", "-q", "origin", "main"]);
        }
        fn git(&self) -> Git {
            Git::new(self.work())
        }
    }

    fn source(commit: &str) -> PublishedSource {
        PublishedSource {
            commit: commit.to_string(),
            verified_at: "2026-09-22T23:37:28+00:00".to_string(),
        }
    }

    /// Peers pushed after `pub publish`: the cut tree still goes to the published commit,
    /// their code stays on main and out of the tree, and the operator's checkout stays put.
    #[test]
    fn peer_pushes_after_the_publish_neither_block_the_cut_nor_leak_into_it() {
        let repo = Repo::new("peer-pushes");
        let published = repo.commit("publish me", &[("src/lib.rs", "v1")]);
        repo.publish_origin();
        repo.commit(
            "feat: a peer's code",
            &[("src/lib.rs", "v2"), ("src/new.rs", "x")],
        );
        let tip = repo.commit("docs: a peer's doc", &[("README", "y")]);
        repo.push();

        let checkout = place_published(&repo.git(), &repo.work(), &source(&published))
            .expect("pushes after the publish are not a refusal");
        let tree = repo.root.path().join("work-cut.noindex");
        assert_eq!(checkout.tree, tree, "the cut tree sits beside the checkout");
        assert!(checkout.moved);
        assert_eq!(checkout.main_ahead, 2);
        let in_tree = |args: &[&str]| {
            let out = testing::git()
                .arg("-C")
                .arg(&tree)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        assert_eq!(in_tree(&["rev-parse", "HEAD"]), published);
        assert_eq!(
            fs::read_to_string(tree.join("src/lib.rs")).unwrap(),
            "v1",
            "the cut tree is the published tree"
        );
        assert!(!tree.join("src/new.rs").exists());
        // Negative control: the excluded code really is on main.
        assert_eq!(repo.run(&["show", "origin/main:src/lib.rs"]), "v2");
        // The operator's checkout never moved: still on main, at the tip.
        assert_eq!(repo.run(&["symbolic-ref", "HEAD"]), "refs/heads/main");
        assert_eq!(repo.run(&["rev-parse", "HEAD"]), tip);
        assert_eq!(
            fs::read_to_string(repo.work().join("src/lib.rs")).unwrap(),
            "v2"
        );

        // Already there: nothing moves.
        let again = place_published(&repo.git(), &repo.work(), &source(&published)).unwrap();
        assert!(!again.moved);
        assert_eq!(again.main_ahead, 2);

        // A tree someone deleted by hand is still registered — it is recreated.
        fs::remove_dir_all(&tree).unwrap();
        let recreated = place_published(&repo.git(), &repo.work(), &source(&published)).unwrap();
        assert!(recreated.moved);
        assert_eq!(in_tree(&["rev-parse", "HEAD"]), published);
    }

    /// Changing ignore rules or tracking an ignored path does not establish who
    /// wrote its files: both artifacts and personal notes survive a refused move.
    /// A move with neither conflict succeeds and retains its ignored warm build.
    #[test]
    fn a_move_preserves_ignored_work_when_rules_or_tracked_paths_change() {
        // The cases share nothing (each its own repository and origin), so they run at once.
        let run = |(case, leftovers, replacement): (&str, bool, bool)| {
            let mut failures = Vec::new();
            let repo = Repo::new("leftovers");
            repo.commit(
                "a tool with its own ignore file",
                &[
                    (".gitignore", "/target/\n"),
                    ("tools/old/.gitignore", "out/\nCargo.lock\nNOTES\n"),
                    ("tools/old/src.rs", "x"),
                    ("src/lib.rs", "v1"),
                ],
            );
            repo.publish_origin();
            let first = repo.run(&["rev-parse", "HEAD"]);
            let tree = place_published(&repo.git(), &repo.work(), &source(&first))
                .unwrap()
                .tree;
            let files = [
                ("tools/old/out/model.tla", "spec"),
                ("tools/old/Cargo.lock", "lock"),
                ("tools/old/NOTES", "personal note"),
            ];
            for (rel, body) in files
                .iter()
                .copied()
                .filter(|_| leftovers)
                .chain([("target/release/aterm", "warm")])
            {
                fs::create_dir_all(tree.join(rel).parent().unwrap()).unwrap();
                fs::write(tree.join(rel), body).unwrap();
            }
            repo.run(&["rm", "-r", "-q", "tools/old"]);
            let mut changed = vec![("src/lib.rs", "v2")];
            if replacement {
                changed.push(("tools/old/NOTES", "new tracked contents"));
            }
            let second = repo.commit("the tool left the tree", &changed);
            repo.push();
            let placed = place_published(&repo.git(), &repo.work(), &source(&second));
            if leftovers {
                if placed.is_ok() {
                    failures.push(format!(
                        "{case}: admitted a move that conflicts with ignored work"
                    ));
                }
                for (rel, body) in files {
                    let actual = fs::read_to_string(tree.join(rel));
                    if actual.as_deref().ok() != Some(body) {
                        failures.push(format!("{case}: {rel} lost {body:?}: {actual:?}"));
                    }
                }
            } else {
                assert!(placed.expect("a clean move succeeds").moved);
                assert!(!tree.join("tools/old").exists());
            }
            assert_eq!(
                fs::read_to_string(tree.join("target/release/aterm")).unwrap(),
                "warm",
                "a still-ignored build stays byte for byte"
            );
            let expected_head = if replacement { &first } else { &second };
            let actual_head = rev_parse(&testing::Git::new(&tree), "HEAD").unwrap();
            if actual_head != *expected_head {
                failures.push(format!(
                    "{case}: moved HEAD despite an ignored-file replacement"
                ));
            }
            let observed_source = fs::read_to_string(tree.join("src/lib.rs")).unwrap();
            let expected_source = if replacement { "v1" } else { "v2" };
            if observed_source != expected_source {
                failures.push(format!(
                    "{case}: tracked source moved to {observed_source:?}"
                ));
            }
            // Work already exposed before the move refuses without moving HEAD.
            fs::write(tree.join("NOTES"), "mine").unwrap();
            let third = repo.commit("another", &[("src/lib.rs", "v3")]);
            repo.push();
            let error = place_published(&repo.git(), &repo.work(), &source(&third))
                .expect_err("pre-existing work refuses")
                .to_string();
            assert!(error.contains("has changes"), "{error}");
            assert_eq!(fs::read_to_string(tree.join("NOTES")).unwrap(), "mine");
            assert_eq!(
                rev_parse(&testing::Git::new(&tree), "HEAD").unwrap(),
                actual_head
            );
            failures
        };
        let failures: Vec<String> = std::thread::scope(|scope| {
            let running: Vec<_> = [
                ("clean move", false, false),
                ("exposed ignored files", true, false),
                ("ignored note becomes tracked", true, true),
            ]
            .into_iter()
            .map(|each| scope.spawn(move || run(each)))
            .collect();
            running
                .into_iter()
                .flat_map(|ran| ran.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
                .collect()
        });
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn a_published_commit_main_does_not_carry_is_refused() {
        let repo = Repo::new("off-main");
        repo.commit("base", &[("a", "1")]);
        repo.publish_origin();
        repo.run(&["checkout", "-q", "-b", "side"]);
        let side = repo.commit("a side commit", &[("a", "2")]);
        repo.run(&["checkout", "-q", "main"]);
        place_published(&repo.git(), &repo.work(), &source(&side))
            .expect_err("a commit main does not carry is no release source");
        assert!(
            !repo.root.path().join("work-cut.noindex").exists(),
            "no cut tree was made"
        );

        let unknown = "f".repeat(40);
        place_published(&repo.git(), &repo.work(), &source(&unknown))
            .expect_err("a commit this repository does not have");
    }

    /// The operator's uncommitted work neither blocks the cut nor is touched by it; changes
    /// inside the cut tree refuse, since the cutter never discards work it did not make.
    #[test]
    fn the_operators_work_does_not_block_the_cut_and_a_dirty_cut_tree_refuses() {
        let repo = Repo::new("dirty");
        let first = repo.commit("publish me", &[("a", "1")]);
        repo.publish_origin();
        let second = repo.commit("later", &[("a", "2")]);
        repo.push();
        fs::write(repo.work().join("a"), "uncommitted").unwrap();
        let checkout = place_published(&repo.git(), &repo.work(), &source(&first))
            .expect("the operator's uncommitted work is not the cut's business");
        assert_eq!(
            fs::read_to_string(repo.work().join("a")).unwrap(),
            "uncommitted",
            "and it is left exactly as it was"
        );

        fs::write(checkout.tree.join("a"), "edited in the cut tree").unwrap();
        place_published(&repo.git(), &repo.work(), &source(&second))
            .expect_err("a cut tree with changes must refuse before it moves");
        assert_eq!(
            fs::read_to_string(checkout.tree.join("a")).unwrap(),
            "edited in the cut tree",
            "the change was not discarded"
        );

        // One judge: a file `.git/info/exclude` hides from `git status` is still a change
        // to the cut tree, as `clean_tree` would refuse it right after.
        let in_tree = |args: &[&str]| {
            let out = testing::git()
                .arg("-C")
                .arg(&checkout.tree)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        in_tree(&["checkout", "-q", "--", "a"]);
        fs::write(checkout.tree.join("stray.txt"), "left here").unwrap();
        let info = repo.work().join(".git/info");
        fs::create_dir_all(&info).unwrap();
        fs::write(info.join("exclude"), "stray.txt\n").unwrap();
        assert_eq!(
            in_tree(&["status", "--porcelain"]),
            "",
            "porcelain sees nothing"
        );
        let error = place_published(&repo.git(), &repo.work(), &source(&second))
            .expect_err("a file only info/exclude hides is a change")
            .to_string();
        assert!(error.contains("stray.txt"), "{error}");
        fs::remove_file(checkout.tree.join("stray.txt")).unwrap();
        place_published(&repo.git(), &repo.work(), &source(&second))
            .expect("a clean cut tree moves");
    }

    /// A directory at the cut tree's path that is not this repository's worktree is
    /// never touched — the cutter refuses and names it.
    #[test]
    fn a_foreign_directory_where_the_cut_tree_goes_is_refused_and_left_alone() {
        let repo = Repo::new("foreign");
        let published = repo.commit("publish me", &[("a", "1")]);
        repo.publish_origin();
        let foreign = repo.root.path().join("work-cut.noindex");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("keep"), "mine").unwrap();
        place_published(&repo.git(), &repo.work(), &source(&published)).expect_err("not ours");
        assert_eq!(fs::read_to_string(foreign.join("keep")).unwrap(), "mine");
    }

    #[test]
    fn the_cut_tree_sits_beside_the_checkout_never_inside_it() {
        assert_eq!(
            cut_tree_path(Path::new("/Users//me/aterm")).unwrap(),
            PathBuf::from("/Users//me/aterm-cut.noindex")
        );
        assert!(cut_tree_path(Path::new("/")).is_err());
    }

    #[test]
    fn the_newest_aterm_row_is_the_published_source() {
        let sha = |c: char| c.to_string().repeat(40);
        let text = format!(
            r#"{{"schema":1,"mappings":{{
                "aterm":{{
                    "{}":{{"verified_at":"2026-09-21T19:38:24+00:00"}},
                    "{}":{{"verified_at":"2026-09-22T23:37:28+00:00"}},
                    "short":{{"verified_at":"2026-09-30T00:00:00+00:00"}}
                }},
                "trust":{{"{}":{{"verified_at":"2026-12-31T00:00:00+00:00"}}}}
            }}}}"#,
            sha('a'),
            sha('b'),
            sha('c')
        );
        let newest = newest_published_source(&text).unwrap();
        assert_eq!(
            newest.commit,
            sha('b'),
            "another repo's row and a non-sha key never win"
        );
        assert!(newest_published_source(r#"{"mappings":{}}"#).is_err());
        assert!(newest_published_source("not json").is_err());
    }

    /// THE SOURCE IS NEVER PUBLISHED AHEAD OF A BINARY THAT CANNOT FOLLOW IT,
    /// against real git: `ship check` judges the COMMIT it is
    /// named — its objects and the records for it, never this checkout — with
    /// the cut's own gates in the cut's order, runs every gate after the
    /// cutter identity whatever the one before it said, and names each one
    /// that would refuse, in the cut's words. With the predecessor's fixtures,
    /// release notes and a handoff policy in the commit, its bundle built in this
    /// clone (the release journal names it) with a verified record naming the bundle's
    /// executable ([`verified`]), it passes, however far HEAD and the working
    /// tree have moved.
    #[test]
    fn ship_check_judges_the_named_commit_with_the_cuts_own_gates() {
        use aterm_update_core::handoff_policy::SOURCE_PATH;
        let repo = Repo::new("precheck");
        let cargo = "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"0.2.0\"\n";
        let guard = "const PINNED_DESKS: &[(&str, &str)] = &[\n    (\"v0.1.0\", \"history\"),\n];\n\
                     const REQUIRED_DESKS: &[(&str, &str)] = &[\n    (\"v0.1.0\", \"history\"),\n];\n";
        let notes = "# Changelog\n\n## [Unreleased]\n\n- a change worth releasing\n\n\
                     ## [0.1.0]\n\n- the first one\n";
        let fixtures = format!("{HANDOFF_FIXTURE_ROOT}/v0.1.0");
        let desk = format!("{fixtures}/history/parent.toml");
        let candidate = repo.commit(
            "the release candidate",
            &[
                ("Cargo.toml", cargo),
                (crate::ledger::LEDGER_FILE, "# ledger\n1790000000 0.1.0\n"),
                (HANDOFF_FIXTURE_GUARD, guard),
                (&desk, "producer_version = \"0.1.0\"\n"),
                (changelog::CHANGELOG_FILE, notes),
                (SOURCE_PATH, "schema = 1\n"),
            ],
        );
        repo.publish_origin();
        repo.run(&["tag", "v0.1.0"]);
        repo.run(&["push", "-q", "origin", "v0.1.0"]);
        // A peer's commit on top, which drops the fixtures, and a working tree whose notes
        // and policy no cut could follow: neither is judged for the candidate.
        repo.run(&["rm", "-q", "-r", &fixtures]);
        let later = repo.commit("a peer's commit", &[]);
        for (path, body) in [
            (changelog::CHANGELOG_FILE, "# Changelog\n"),
            (SOURCE_PATH, "schema = 1\ncary = \"full\"\n"),
        ] {
            fs::write(repo.work().join(path), body).unwrap();
        }
        let git = repo.git();
        // A check by the commit's own cutter: built from the commit it judges.
        let check = |rev: &str| {
            let own = repo.run(&["rev-parse", rev]);
            precheck_with(&git, rev, Ok(&own)).expect("a releasable commit")
        };

        // Nothing verified: the record gate refuses — with the cut's own refusal
        // and remedy — and the gates that read the candidate's objects pass.
        let found = check(&candidate);
        assert_eq!(
            (found.commit.as_str(), found.version.as_str()),
            (candidate.as_str(), "0.2.0")
        );
        assert_eq!(found.refused(), ["verify release"]);

        // What the cut reads: the candidate's bundle, named by this clone's release
        // journal, and the record that `verify release` passed on its executable.
        let macho = "ab".repeat(32);
        crate::release::Journal {
            version: "0.2.0".to_string(),
            commit: candidate.clone(),
            artifact: Some(crate::release::Artifact {
                key: "cd".repeat(32),
                app: repo.root.path().join("cache/aterm.app"),
                macho_sha256: macho.clone(),
                build: 1_790_000_100,
            }),
            ..crate::release::Journal::default()
        }
        .save(
            &repo
                .work()
                .join(".git")
                .join(crate::release::RELEASE_DIR)
                .join(crate::release::JOURNAL_FILE),
        )
        .unwrap();
        record_verified(&git, &candidate, &macho).unwrap();
        let found = check(&candidate);
        assert!(found.refused().is_empty(), "{}", found.transcript());

        // Negative controls: HEAD is judged by its own objects and records (the peer's
        // commit dropped the fixtures, and nothing verified it), not the working tree's.
        let head = check("HEAD");
        assert_eq!(head.commit, later);
        assert_eq!(head.refused(), ["handoff fixtures", "verify release"]);
        // The bytes, not the commit: a record about other bytes verifies nothing.
        record_verified(&git, &candidate, &"ef".repeat(32)).unwrap();
        let found = check(&candidate);
        assert_eq!(
            found.refused(),
            ["verify release"],
            "{}",
            found.transcript()
        );
        // A version no release can carry, and a name that is no commit, are
        // refused before any gate.
        let patch = repo.commit(
            "a patch version",
            &[("Cargo.toml", &cargo.replace("0.2.0", "0.2.1"))],
        );
        precheck_with(&git, &patch, Ok(&patch)).expect_err("no release version");
        assert!(precheck_with(&git, "no-such-commit", Ok(&patch)).is_err());
    }

    /// A rescue a release sealed stays in every later one (RELEASING.md, "The handoff policy
    /// a release seals"): dropping or narrowing it refuses, keeping or widening passes,
    /// retiring its exact range under `[[retired]]` passes and says so, and a `[[retired]]`
    /// entry naming a kept rescue refuses. The seal is read at origin's tag, never this
    /// clone's.
    #[test]
    fn a_cut_keeps_the_handoff_policy_its_predecessor_sealed() {
        use aterm_update_core::handoff_policy::SOURCE_PATH;
        let repo = Repo::new("sealed-policy");
        // The predecessor seals one range in the pre-`[[ranges]]` shape; this cut writes
        // each rescue as a `[[ranges]]` table under its hull.
        let knob = |range: &str| {
            format!("schema = 1\napplies_to_producers = {range}\ncarry = \"repaint\"\n")
        };
        let ranged = |range: &str| {
            format!(
                "{}\n[[ranges]]\nproducers = {range}\ncarry = \"repaint\"\nreason = \"the \
                 rescue\"\n",
                knob(range)
            )
        };
        let retired = |range: &str| {
            format!(
                "\n[[retired]]\nproducers = {range}\nreason = \"every build in range is past its \
                 last supported update\"\ndate = \"2026-10-03\"\n"
            )
        };
        let rescue = knob("[1791000000, 1791500000]");
        repo.commit(
            "v0.1.0, which seals a rescue",
            &[
                (
                    crate::ledger::LEDGER_FILE,
                    "# ledger\n1791000000 0.0.8\n1791500000 0.0.9\n1792000000 0.1.0\n",
                ),
                (SOURCE_PATH, rescue.as_str()),
            ],
        );
        repo.publish_origin();
        repo.run(&["tag", "v0.1.0"]);
        repo.run(&["push", "-q", "origin", "v0.1.0"]);
        let git = repo.git();
        let judge = |policy: &str| {
            let commit = repo.commit(policy, &[(SOURCE_PATH, policy)]);
            handoff_policy_gate_at(&git, &commit, "0.2.0").map(|p| p.to_string())
        };

        for kept in [
            ranged("[1791000000, 1791500000]"),
            ranged("[1790000000, 1792000000]"),
        ] {
            judge(&kept).unwrap_or_else(|e| panic!("{kept}: {e}"));
        }
        for dropped in [
            "schema = 1\n".to_string(),
            ranged("[1791000001, 1791500000]"),
            ranged("[1791000000, 1791500000]").replace("repaint", "visible"),
        ] {
            judge(&dropped).expect_err(&dropped);
        }

        // Retired on purpose: the exact sealed range, a reason and a date, stated
        // in the line the transcript prints.
        let said = judge(&format!(
            "schema = 1\n{}",
            retired("[1791000000, 1791500000]")
        ))
        .expect("a named retirement drops the rescue");
        assert!(said.contains("RETIRES"), "{said}");
        for misnamed in [
            format!("schema = 1\n{}", retired("[1791000000, 1791499999]")),
            format!(
                "{}{}",
                ranged("[1790000000, 1792000000]"),
                retired("[1791000000, 1791500000]")
            ),
            format!(
                "{}{}",
                ranged("[1791000000, 1791500000]"),
                retired("[1791000000, 1791500000]")
            ),
        ] {
            judge(&misnamed).expect_err(&misnamed);
        }

        // Abandoned and recut from another clone: origin's v0.1.0 now seals a wider
        // rescue, while this clone's v0.1.0 (a fetch never moves an existing tag) names
        // the first attempt. Origin's shipped, so keeping only the first range drops it.
        let other = repo.root.path().join("other");
        fs::create_dir_all(&other).unwrap();
        let in_other = |args: &[&str]| {
            let out = testing::git()
                .arg("-C")
                .arg(&other)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let origin = repo.root.path().join("origin.git");
        in_other(&["clone", "-q", origin.to_str().unwrap(), "."]);
        in_other(&["config", "user.name", "Test"]);
        in_other(&["config", "user.email", "test@example.invalid"]);
        in_other(&["config", "commit.gpgsign", "false"]);
        in_other(&["config", "tag.gpgsign", "false"]);
        fs::write(other.join(SOURCE_PATH), knob("[1791000000, 1792000000]")).unwrap();
        in_other(&["commit", "-q", "-a", "-m", "v0.1.0, recut"]);
        // Annotated, as the cut mints it: origin lists the tag and its commit.
        in_other(&["tag", "-f", "-a", "-m", "v0.1.0", "v0.1.0"]);
        in_other(&["push", "-q", "--force", "origin", "refs/tags/v0.1.0"]);
        judge(&ranged("[1791000000, 1791500000]")).expect_err("origin's tag, not this clone's");

        // A predecessor whose tree holds no policy file sealed nothing. The ledger is
        // RELEASES.ledger's own: v0.95.0, cut before the policy reader, and v0.97.0, the
        // first producer that follows a policy.
        let bare = Repo::new("sealed-policy-none");
        bare.commit(
            "v0.97.0, with no policy file",
            &[(
                crate::ledger::LEDGER_FILE,
                "# ledger\n1790563767 0.95.0\n1790585188 0.97.0\n",
            )],
        );
        bare.publish_origin();
        bare.run(&["tag", "v0.97.0"]);
        bare.run(&["push", "-q", "origin", "v0.97.0"]);
        let judge = |policy: &str| {
            let commit = bare.commit(policy, &[(SOURCE_PATH, policy)]);
            handoff_policy_gate_at(&bare.git(), &commit, "0.98.0").map(|p| p.to_string())
        };
        judge("schema = 1\n").expect("nothing sealed");
        // With nothing to keep, a knob's range must still name builds that can hand over
        // to this cut and follow a policy: one naming no ledger build (a digit dropped),
        // or only v0.95.0's (cut before the reader), rescues nothing, and one reaching
        // past v0.97.0's build binds every later release's producer.
        judge(&ranged("[1790563767, 1790585188]")).expect("names v0.97.0's build");
        for range in [
            "[179000000, 179000000]",
            "[1790563767, 1790563767]",
            "[1790585188, 1890585188]",
        ] {
            judge(&ranged(range)).expect_err(range);
        }
        // A `[[retired]]` table naming nothing the predecessor sealed is the record
        // of an earlier retirement: kept, and said by no RETIRES line.
        let said = judge(&format!("schema = 1\n{}", retired("[10, 20]")))
            .expect("an earlier retirement, kept on record");
        assert!(!said.contains("RETIRES"), "{said}");
    }

    /// SEVERAL RESCUES, EACH KEPT UNTIL IT IS RETIRED (round eight of the update
    /// audit, r8c finding 504). A predecessor that sealed two rescues with
    /// different knobs, as `[[ranges]]` under their hull: a cut that keeps both
    /// passes, and so does one that keeps them inside wider or more conservative
    /// rescues. A cut that silently drops either one is refused before the claim,
    /// naming the dropped builds and the reason the predecessor sealed them for;
    /// one that lists it under `[[retired]]` with a reason and a date passes and
    /// says so.
    #[test]
    fn a_cut_that_drops_the_predecessors_producer_policy_is_refused() {
        use aterm_update_core::handoff_policy::SOURCE_PATH;
        const LEDGER: &str = "# ledger\n1791000000 0.0.8\n1791500000 0.0.9\n1792000000 0.1.0\n";
        const PANICS: &str = "[[ranges]]\nproducers = [1791000000, 1791100000]\ncarry = \
                              \"repaint\"\nreason = \"v0.0.8's capture panics on a wide desk\"\n";
        const GATE: &str = "[[ranges]]\nproducers = [1791500000, 1791900000]\n\
                            park_quiet_gate_at_land = \"relaxed\"\nreason = \"v0.0.9's Land \
                            gate never opens\"\n";
        const HULL: &str = "schema = 1\napplies_to_producers = [1791000000, 1791900000]\ncarry = \
                            \"repaint\"\npark_quiet_gate_at_land = \"relaxed\"\n";
        let sealed = format!("{HULL}\n{PANICS}\n{GATE}");
        let repo = Repo::new("sealed-ranges");
        repo.commit(
            "v0.1.0, which seals two rescues",
            &[
                (crate::ledger::LEDGER_FILE, LEDGER),
                (SOURCE_PATH, sealed.as_str()),
            ],
        );
        repo.publish_origin();
        repo.run(&["tag", "v0.1.0"]);
        repo.run(&["push", "-q", "origin", "v0.1.0"]);
        let git = repo.git();
        let judge = |policy: &str| {
            let commit = repo.commit(policy, &[(SOURCE_PATH, policy)]);
            handoff_policy_gate_at(&git, &commit, "0.2.0").map(|p| p.to_string())
        };

        judge(&sealed).unwrap_or_else(|e| panic!("both kept: {e}"));
        // Kept inside a wider rescue, and by a more conservative one.
        let wider = "schema = 1\napplies_to_producers = [1791000000, 1791900000]\ncarry = \
                     \"repaint\"\npark_quiet_gate_at_land = \"relaxed\"\n\n[[ranges]]\nproducers \
                     = [1791000000, 1791900000]\ncarry = \"repaint\"\npark_quiet_gate_at_land = \
                     \"relaxed\"\nreason = \"both\"\n";
        judge(wider).unwrap_or_else(|e| panic!("one wider rescue keeps both: {e}"));

        // Dropped silently: refused, naming the builds and why they were rescued.
        let only_panics = format!(
            "schema = 1\napplies_to_producers = [1791000000, 1791100000]\ncarry = \
             \"repaint\"\n\n{PANICS}"
        );
        judge(&only_panics).expect_err("a dropped rescue");
        // A narrowed or weakened rescue drops builds or knobs just the same, each
        // under the hull it now has.
        let narrowed = format!(
            "schema = 1\napplies_to_producers = [1791000000, 1791800000]\ncarry = \
             \"repaint\"\npark_quiet_gate_at_land = \"relaxed\"\n\n{PANICS}\n{}",
            GATE.replace("1791900000", "1791800000")
        );
        let weakened = format!(
            "schema = 1\napplies_to_producers = [1791000000, 1791900000]\ncarry = \
             \"visible\"\npark_quiet_gate_at_land = \"relaxed\"\n\n{}\n{GATE}",
            PANICS.replace("repaint", "visible")
        );
        for changed in [narrowed, weakened] {
            judge(&changed).expect_err(&changed);
        }

        // Retired on purpose: the exact range, a reason and a date, said in the
        // line the transcript prints.
        let retired = format!(
            "{only_panics}\n[[retired]]\nproducers = [1791500000, 1791900000]\nreason = \"every \
             v0.0.9 install is past its last supported update\"\ndate = \"2026-10-03\"\n"
        );
        let said = judge(&retired).expect("a retired rescue may be dropped");
        assert!(said.contains("RETIRES"), "{said}");
    }

    /// `ship check` answers the cut's gates only with the commit's own cutter, whose rules
    /// are the judged commit's: any other check runs the identity gate alone, NOT RUN with
    /// the remedy, and exits 3.
    #[test]
    fn ship_check_answers_only_with_the_commits_own_cutter() {
        let repo = Repo::new("precheck-identity");
        let cargo = "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"0.2.0\"\n";
        let candidate = repo.commit(
            "the release candidate",
            &[
                ("Cargo.toml", cargo),
                ("crates/aterm-release/src/gates.rs", "// the rules\n"),
            ],
        );
        let git = repo.git();
        // The commit's own cutter answers, and says so as its first gate.
        let found = precheck_with(&git, &candidate, Ok(&candidate)).expect("its own");
        assert_eq!(found.gates[0].name, "cutter identity");
        assert!(matches!(found.gates[0].outcome, Outcome::Pass(_)));
        // NEGATIVE CONTROLS: other rules answer nothing — a checker built before the
        // judged commit (even one that moved only a doc)...
        let docs = repo.commit("a doc", &[("docs/NOTE.md", "prose\n")]);
        let not_run = |stamp: std::result::Result<&str, &crate::cutter::NotOwn>| {
            let found = precheck_with(&git, &docs, stamp).expect("the commit reads");
            assert_eq!(
                found.not_run(),
                ["cutter identity"],
                "{}",
                found.transcript()
            );
            assert_eq!(found.gates.len(), 1, "no cut gate is asked");
            assert_eq!(found.exit_code(), 3);
            match &found.gates[0].outcome {
                Outcome::CouldNotRun(r) => r.clone(),
                other => panic!("{other:?}"),
            }
        };
        not_run(Ok(&candidate));
        // ...one that names no commit, or one that is no commit's build, which names its
        // own remedy.
        not_run(Ok("unknown"));
        let none = crate::cutter::NotOwn {
            why: "a source changed after this cutter was linked".into(),
            fix: "relink it".into(),
        };
        let r = not_run(Err(&none));
        assert!(r.why.contains(&none.why), "{r:?}");
        assert_eq!(r.remedy, none.fix);
    }
}

#[cfg(test)]
mod trust_from_source_tests {
    use super::*;

    /// The from-source remedy names no home folder (a Trust checkout is wherever it was
    /// cloned) and is atpkg's own command (`Toolchain::INSTALL_REMEDY`), so a release gate
    /// and the doctor never give two remedies for one missing toolchain.
    #[test]
    fn the_from_source_remedy_names_no_home_folder_and_is_atpkgs_command() {
        for remedy in [TRUST_FROM_SOURCE, TRUST_TOOLCHAIN_REMEDIES] {
            assert!(!remedy.contains('~'), "a home folder: {remedy}");
            assert!(remedy.contains("in a Trust checkout"), "{remedy}");
        }
        let command = |remedy: &'static str| {
            let at = remedy
                .find("python3 scripts/recreate_bootstrap.py")
                .unwrap_or_else(|| panic!("no recreator: {remedy}"));
            &remedy[at..]
        };
        assert_eq!(
            command(TRUST_FROM_SOURCE),
            command(atpkg::toolchain::Toolchain::INSTALL_REMEDY)
        );
    }
}
