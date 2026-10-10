// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pre-claim gates (release spec §6 `gates.rs`, plus the changelog gates of
//! §3): macOS arm64 host, clean tree, HEAD == the published commit (a real
//! cut — [`place_published`] put the cut tree there), tag absent local+remote,
//! changelog non-empty/no-`'''`, the previous release's handoff fixtures
//! checked in and pinned (its whole required desk set, from the guard's
//! floor on), the handoff policy the bundle seals, `gh auth
//! status`, Trust rustc
//! probe (always on — the repo compiles with Trust, there is no opt-out
//! lane), disk space, and last the cross-version handoff guard run in the
//! cut tree (the one gate that takes minutes). All fail closed BEFORE anything
//! is committed or pushed — a failed gate costs seconds (or those minutes),
//! not a burned ledger number.
//!
//! Git-backed gates go through the injectable [`GitRunner`] seam (same one the
//! claim uses); host-tool gates (`gh`, `rustup`, `df`, the Trust rustc) shell
//! out directly — they interrogate THIS machine, which is exactly what the
//! gate is for.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::changelog;
use crate::channel;
use crate::ledger::{Error, GitRunner, Result, git_ok, rev_parse};

/// Free-disk floor for the release's build and its cut. The build carries two full
/// `--release` target trees in the release cache (Trust arm64 + rustup x86_64, both
/// with `CARGO_PROFILE_RELEASE_DEBUG=1` for the dSYM) and keeps its bundle there; the
/// cut stages a copy of it, the DMG and the zip in `dist/` — 10 GiB is a conservative
/// floor that fails BEFORE a build dies at 99% on ENOSPC.
pub const MIN_FREE_DISK_GIB: u64 = 10;

/// The Trust toolchain's stage2 tool dir (`targo`, `trustc`, `trustdoc`, …), in the
/// ONE resolution order every gate in this workspace uses (`aterm_verify::toolchain`,
/// mirrored here because this crate does not depend on that one):
/// `$TRUST_STAGE2_BIN` (an explicit development override, never fallen back from) →
/// the rustup `trust` toolchain → the atpkg store's `store/trust/current/bin` →
/// `PATH`, the rustup entry ranked BELOW the store when it is older than the store's
/// build (`discovery_order`). A candidate must carry `targo` AND `trustc`. No build
/// tree is probed: a from-source toolchain is reached SEALED, through the rustup entry
/// Trust's `scripts/promote-toolchain.sh` flips onto the seal.
/// Resolved to the PHYSICAL path — the protected Trust drivers refuse a symlinked
/// toolchain path — and, where the rustup entry is atpkg's VIEW (rebuilt in place on
/// every update), to the build that view presents, so a pin cannot change under a cut.
/// The gates resolve the trust-named binaries directly rather than a PATH `cargo`, so
/// correctness does not depend on the operator's rustup state. Every host lane runs
/// `targo --unverified …` FROM THIS DIR.
pub fn trust_stage2_bin() -> Result<PathBuf> {
    // PINNED ONCE PER PROCESS. The candidate walk ends at the atpkg store's
    // `store/trust/current`, a MUTABLE indirection (`aterm pkg install trust`
    // and `promote-toolchain.sh` repoint it), and a half-hour cut calls this
    // from several places. Resolving a moving name more than once can split a
    // run across two compilers with nothing going red; resolving once to the
    // PHYSICAL path keeps a concurrent seal out of a running cut.
    //
    // Only SUCCESS is pinned. A failure caches nothing — there is no toolchain
    // identity to protect in that case, and a sticky "no toolchain" would
    // outlive an install the operator performs on the advice of this very
    // error.
    let pinned = pin_first_success(&PINNED, resolve_trust_stage2_bin)?;
    if LEASE_ARMED.load(std::sync::atomic::Ordering::Relaxed) {
        hold_toolchain_lease(&pinned);
    }
    Ok(pinned)
}

/// The one toolchain this process pinned ([`trust_stage2_bin`]).
static PINNED: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Pin `dir` as this process's Trust toolchain before anything resolves one: `ship
/// release` holds ONE prover for its whole life — the one its journal names on a
/// resume, which the store's current build may no longer be — so every in-process
/// judge that asks [`trust_stage2_bin`] (the provisioning audit's toolchain,
/// verifier and front-door lines among them) judges that prover, never the store's
/// newer build.
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

/// Whether this process is a CUT ([`arm_toolchain_lease`]): only a cut leases the
/// toolchain it pins. The tests resolve the machine's real toolchain through
/// [`trust_stage2_bin`] too, and must never write into the machine's package store.
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

/// THE CUT'S LEASE on the toolchain it pinned ([`atpkg::lease`]), taken once, at the
/// first pin after [`arm_toolchain_lease`], and held for the life of the cut. The pin
/// makes a concurrent seal unable to reach a running cut; the lease makes the package
/// manager unable to take the pinned build away under it: atpkg's gc keeps a leased build, the
/// seam never re-lays a leased rustup view, and an unattended trust update waits while
/// the live toolchain is leased (four hours at the most). Between two of the cut's steps
/// no process runs from the directory, so without the lease nothing said it was in use.
///
/// A toolchain outside the atpkg prefix takes none (nothing of atpkg's reclaims it), and
/// one that cannot be taken is said and never stops the cut. Held in a static, so it is
/// never dropped: the kernel releases the lock when the cutter exits, and atpkg reaps the
/// file of a holder that is gone at its next reading.
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

/// Return the cell's value, resolving it exactly once and only on success.
///
/// Split out so the pinning is a test rather than a promise: a second call with
/// a DIFFERENT resolver must still answer the first one's path, which is the
/// whole property — a mid-run seal cannot change what a running release compiles
/// or judges with.
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
    let (explicit, candidates) = trust_stage2_candidates();
    let mut tried = Vec::new();
    for dir in candidates {
        match fs::canonicalize(&dir) {
            Ok(resolved)
                if resolved.join("trustc").is_file() && resolved.join("targo").is_file() =>
            {
                return Ok(resolved);
            }
            _ => tried.push(dir.display().to_string()),
        }
    }
    // FAULT ONLY, plus the one fact the operator cannot derive: where it looked. The
    // remedies live in [`TRUST_TOOLCHAIN_REMEDIES`] so that the caller can lay them out
    // under its own `fix:` / `or:`.
    Err(Error::new(if explicit {
        format!(
            "no Trust toolchain at TRUST_STAGE2_BIN={} (an explicit override is never fallen \
             back from)",
            tried.join(", ")
        )
    } else {
        format!("no Trust toolchain found\nlooked in: {}", tried.join(", "))
    }))
}

/// The way past a missing x86_64 slice, for a caller to append VERBATIM.
///
/// One remedy text, laid out by whichever grid prints it, so the probe's own error never
/// repeats it under a second `fix:` marker.
pub const X86_SLICE_REMEDIES: &str = "rustup +stable target add x86_64-apple-darwin";

/// How to build Trust from source, for every remedy here that offers it: from the
/// pinned Trust seed, then SEALED. A macro so [`TRUST_TOOLCHAIN_REMEDIES`] can
/// `concat!` it; everything else names [`TRUST_FROM_SOURCE`].
///
/// Never a bare `python3 x.py build --stage 2`: that runs whatever stage0 the
/// checkout's gitignored `bootstrap.toml` names, and until 2026-09-29 `$HOME/trust`'s named
/// a "genesis" stage0 of shell scripts running stock rustup Rust under Trust names. The
/// recreator's `--require-seed --fresh-seed` takes the checksum-pinned Trust seed that
/// trust's `src/stage0` names and refuses rather than wrap a stock compiler (owner,
/// 2026-09-29: no stock Rust in the toolchain for Trust); `--no-register` leaves rustup
/// alone until `promote-toolchain.sh` points it at the seal.
macro_rules! trust_from_source {
    () => {
        "in $HOME/trust, python3 scripts/recreate_bootstrap.py --stage 2 --require-seed \
         --fresh-seed --no-register (stage0 = the pinned Trust seed; it refuses rather than \
         wrap a stock rustc), then seal it with scripts/promote-toolchain.sh"
    };
}

/// The from-source build, for a remedy to name after "from source" (`trust_from_source!`).
pub const TRUST_FROM_SOURCE: &str = trust_from_source!();

/// The three ways to get a Trust toolchain, for a caller to append VERBATIM.
///
/// One text, appended where the caller's layout wants it, so one remedy has one
/// spelling. The package manager leads: it is the ordinary source of the pinned
/// toolchain, and `aterm pkg doctor` names the store it filled. Building from source is
/// the developer alternative.
pub const TRUST_TOOLCHAIN_REMEDIES: &str = concat!(
    "aterm pkg install trust   (then `aterm pkg doctor` to confirm the store)\n\
     or, from source: ",
    trust_from_source!(),
    "\n\
     or point TRUST_STAGE2_BIN at an existing stage2 bin dir"
);

/// The ordered places a Trust toolchain may live, and whether the first is an explicit
/// override (then it is the ONLY one).
fn trust_stage2_candidates() -> (bool, Vec<PathBuf>) {
    if let Some(explicit) = env::var_os("TRUST_STAGE2_BIN") {
        return (true, vec![PathBuf::from(explicit)]);
    }
    let home = aterm_types::dirs::home_dir();
    let configured = atpkg::config::load().prefix_path(home.as_deref());
    let layout = atpkg::store::resolve(configured.as_deref());
    let entry = atpkg::seam::rustup_home()
        .map(|rustup| atpkg::seam::seam_path(&rustup, atpkg::seam::DEFAULT_SEAM));
    let path = env::var_os("PATH");
    (
        false,
        discovery_order(entry.as_deref(), layout.as_ref(), path.as_deref()),
    )
}

/// The walk below the override: the rustup `trust` entry, the store's
/// `store/trust/current/bin`, then `path` — EXCEPT that a rustup entry OLDER than the
/// store's build ranks BELOW the store (`aterm_verify::toolchain::Demoted` is the same
/// exception, mirrored there). The rule is atpkg's own, called here rather than copied:
/// [`atpkg::seam::stale_against_store`], the one `aterm pkg doctor` warns by and `aterm
/// pkg repair` re-points by, so the cutter never ranks a toolchain differently from the
/// verb that names the fix.
///
/// WHY: a hand-made `~/.rustup/toolchains/trust` link to an old stage2 is one atpkg's
/// unattended pass never touches, so without the demotion every cut on the machine
/// compiled with it and no toolchain update reached it ([`atpkg::seam::live_build_tree`]
/// re-points some such links, but not a seal or a link put back after it). Demoted, not
/// dropped: with no store the entry still ranks ahead of PATH.
/// Only an entry outside atpkg's views is weighed — a view already answers with the
/// store's live build (or the dev-linked checkout, which
/// [`atpkg::seam::stale_against_store`] never calls stale).
fn discovery_order(
    entry: Option<&Path>,
    layout: Option<&atpkg::store::Layout>,
    path: Option<&std::ffi::OsStr>,
) -> Vec<PathBuf> {
    let rustup = entry.and_then(|e| rustup_entry_source(e, layout));
    let store = layout.map(|l| l.program_current("trust").join("bin"));
    let stale = match (&rustup, layout) {
        (Some(RustupSource::Foreign(sysroot)), Some(layout)) => {
            atpkg::seam::stale_against_store(layout, sysroot)
        }
        _ => None,
    };
    let rustup = rustup.map(|s| s.sysroot().join("bin"));
    let mut out = Vec::new();
    if stale.is_some() {
        out.extend(store);
        out.extend(rustup);
    } else {
        out.extend(rustup);
        out.extend(store);
    }
    if let Some(path) = path {
        out.extend(env::split_paths(path));
    }
    out
}

/// What the rustup `trust` entry stands for ([`rustup_entry_source`]).
#[derive(Debug, PartialEq, Eq)]
enum RustupSource {
    /// atpkg's VIEW, answered with the sysroot it presents.
    View(PathBuf),
    /// Any other entry — a hand link, the store itself — as its own physical sysroot.
    Foreign(PathBuf),
}

impl RustupSource {
    fn sysroot(&self) -> &Path {
        match self {
            RustupSource::View(p) | RustupSource::Foreign(p) => p,
        }
    }
}

/// The sysroot the rustup `trust` entry stands for. atpkg's VIEW (`<prefix>/rustup/…`)
/// is rebuilt in place on every update, so it answers with what the view presents —
/// the store's live build, or the dev-linked checkout — whose physical path cannot
/// change under a running cut; any other entry (a hand link, the store itself) is
/// itself. `None` when the view presents nothing atpkg can use.
fn rustup_entry_source(
    entry: &Path,
    layout: Option<&atpkg::store::Layout>,
) -> Option<RustupSource> {
    let resolved = fs::canonicalize(entry).ok()?;
    let Some(layout) = layout else {
        return Some(RustupSource::Foreign(resolved));
    };
    let views = fs::canonicalize(atpkg::seam::views_root(layout)).ok();
    if !views.is_some_and(|v| resolved.starts_with(v)) {
        return Some(RustupSource::Foreign(resolved));
    }
    match atpkg::seam::view_source(layout) {
        atpkg::seam::ViewSource::Store => {
            Some(RustupSource::View(atpkg::seam::store_current(layout)))
        }
        atpkg::seam::ViewSource::Linked(checkout) => Some(RustupSource::View(checkout)),
        atpkg::seam::ViewSource::LinkedNoSysroot(_) => None,
    }
}

/// The `targo` build driver from the stage2 tool dir. All native-lane builds and
/// metadata queries go through THIS binary — never a PATH `cargo`, which resolves
/// to a rustup shim with nothing behind it.
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
    /// The notes are already rolled: the checkout's changelog carries the
    /// version's `## [version]` section, so the changelog gate judges THAT section
    /// — the `[Unreleased]` scaffold above it is legitimately empty. False on every
    /// ordinary cut, whose published commit ships its `[Unreleased]` notes.
    pub recut: bool,
    /// This cut publishes nowhere the real channel can be compared against —
    /// `--dry-run` (no uploads at all) or `--rehearse OWNER/REPO` (uploads to a
    /// scratch repo). The channel-version gate is inapplicable then, because the
    /// public channel is not the destination and cannot be expected to carry
    /// this version.
    ///
    /// DERIVED FROM THE CLI FLAGS, never from the environment: an exported
    /// variable could silently disable the gate for a REAL cut.
    pub offline: bool,
    /// Only a true `--dry-run` may execute from a cutter older than the tree.
    /// A rehearsal mutates its scratch repository, so it must use the tree's
    /// own binary just like a production cut.
    pub allow_stale_cutter: bool,
    /// A real cut's published commit, already placed in the cut tree
    /// ([`place_published`]); `None` for a dry run or rehearsal, which build the
    /// checkout as it stands.
    pub published: Option<PublishedCheckout>,
}

/// What the gates learned — everything the cut transcript's `gates` lines
/// print. Informational; the gate *decisions* already happened (any failure
/// returned Err instead).
pub struct GateReport {
    /// Short (8-hex) HEAD sha for the banner.
    pub head_short: String,
    /// Top-level bullet count of the `[Unreleased]` real body.
    pub changelog_entries: usize,
    /// The gh account name, when it could be parsed from `gh auth status`.
    pub gh_account: Option<String>,
    /// The probed trustc path (the Trust stage2 compiler the native build lane
    /// resolves via targo). Always probed — there is no opt-out lane.
    pub trustc: PathBuf,
    /// Free disk in GiB at gate time.
    pub free_disk_gib: u64,
    /// `Some(version)` when the public update channel's source tree was read and
    /// carries exactly this version; `None` when there is no channel configured,
    /// no manifest on it yet, or the gate was explicitly skipped.
    pub channel_version: Option<String>,
    /// How many running processes the staging-liveness gate read — the evidence
    /// behind "nothing runs out of a cut staging bundle".
    pub processes_checked: usize,
    /// The published commit this cut releases — `None` for a dry run or rehearsal.
    pub published: Option<PublishedCheckout>,
    /// How far HEAD is past the newest gate receipt. Stated, never required.
    pub receipts: ReceiptReport,
    /// The release this cut succeeds and the handoff fixtures it has checked in
    /// ([`handoff_fixture_gate`]); `None` only when the ledger records no earlier
    /// release at all, so nothing hands off to this one.
    pub handoff_fixtures: Option<HandoffFixtures>,
    /// How many of the cross-version guard's tests passed in the cut tree
    /// ([`handoff_fixture_guard_gate`]): every shipped producer's desks adopted
    /// by the consumer this cut ships.
    pub fixture_guard_tests: usize,
    /// What the handoff policy this cut will seal into the bundle says, and the
    /// predecessor's rescue it retires, if any ([`handoff_policy_gate`]).
    pub handoff_policy: CutHandoffPolicy,
}

/// Run every gate, in the transcript's order, first failure wins. Cheap and
/// side-effect-free by construction (the one network touch is a fetch).
///
/// `git` and `tree` are the tree the cut releases — the cut tree for a real cut, the
/// checkout as it stands for a dry run or rehearsal; `state` is the operator's
/// checkout, whose `dist/` and gate receipts the cut reads and writes.
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
    // here is the ASSERTION that it worked. A dry run and a rehearsal build the
    // checkout as it stands.
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
    let cl = changelog_gate(
        tree,
        if opts.recut {
            &opts.version
        } else {
            "Unreleased"
        },
    )?;
    // Every flavour runs it — a dry run and a rehearsal too, so that a dry run
    // shows the refusal a real cut would meet — and a recut judges the same
    // predecessor its first attempt did (the version being cut is never its own
    // predecessor). It reads the tree and lists origin's tags; nothing is claimed.
    let handoff_fixtures = handoff_fixture_gate(git, tree, &opts.version)?;
    let handoff_policy = handoff_policy_gate(
        git,
        tree,
        handoff_fixtures
            .as_ref()
            .map(|found| found.release.as_str()),
    )?;
    let gh_account = gh_auth()?;
    locked_metadata_gate(tree)?;
    let trustc = trustc_probe(tree)?;
    let free_disk_gib = disk_gate(tree)?;
    let processes_checked =
        staged_bundle_liveness_gate(&state.join("dist"), &crate::bundle::running_processes)?;
    // How far the commit this cut releases is past the newest gate receipt — stated,
    // never required. Local git, so before the channel's network read. The store is
    // the git common dir's, which the cut tree shares with every other worktree,
    // and a tree receipt also counts from a checkout `aterm-release.receiptsFrom`
    // names ([`Stores`]).
    let receipts = receipt_report(git, &Stores::of(git)?)?;
    // Last, because it is the only gate that talks to the public channel: the cheap
    // local refusals should all have fired before we spend a network round trip.
    let channel_version = channel_version_gate(tree, &opts.version, opts.offline)?;
    // After every other gate, because it is the one that costs minutes: a debug
    // build of `aterm-gui`'s tests in the cut tree. Every cheaper refusal — the
    // compiler, the disk, the channel — has fired first, and it still runs
    // before the claim, so a red guard costs no build number.
    let fixture_guard_tests = handoff_fixture_guard_gate(tree, &mut |command| command.output())?;
    Ok(GateReport {
        head_short: head.chars().take(8).collect(),
        changelog_entries: cl.entries,
        gh_account,
        trustc,
        free_disk_gib,
        channel_version,
        handoff_fixtures,
        fixture_guard_tests,
        handoff_policy,
        processes_checked,
        published: opts.published.clone(),
        receipts,
    })
}

/// Refuse a cut whose checked-in handoff policy (`publish/handoff-policy.toml`) is
/// not one the producers it is meant for could follow — BEFORE the claim, so a typo
/// costs seconds, not a build number.
///
/// The bundle step copies that file into the `.app` before signing, and every
/// older aterm handing its sessions to this release reads it from there. A
/// producer IGNORES a file it cannot interpret (with one log line), which is the
/// right fallback in the field and the wrong one here: a misspelt key or value
/// would ship as a policy nobody follows, and the release it was meant to
/// rescue would behave exactly as if it had none. So the cutter reads it
/// strictly ([`crate::bundle::handoff_policy_for_cut`]): schema 1, only the keys
/// schema 1 defines, only their values, never the reserved `seamless`, each rescue
/// a `[[ranges]]` table with a reason, and the top-level keys their hull.
///
/// Each rescue's range must name builds that can hand over to this cut, and the
/// file must also KEEP every rescue `predecessor` (the release this cut succeeds,
/// from [`handoff_fixture_gate`]) sealed, or retire it by name
/// ([`sealed_policy_kept`]): a rescue stays in every later release while any
/// build it names may still be installed.
pub fn handoff_policy_gate(
    git: &dyn GitRunner,
    repo: &Path,
    predecessor: Option<&str>,
) -> Result<CutHandoffPolicy> {
    let (text, policy) = crate::bundle::handoff_policy_for_cut(repo)
        .map_err(|why| Error::new(format!("handoff-policy gate: {why}")))?;
    sealed_policy_kept(git, &GateTree::Dir(repo), predecessor, policy, &text)
}

/// [`handoff_policy_gate`] for a COMMIT: the policy blob the commit holds, read
/// by the same strict rule ([`crate::bundle::handoff_policy_of_text`]) — the
/// file a cut of it would seal into its bundle — against the policy the release
/// a cut of it succeeds sealed.
///
/// # Errors
/// The commit holds no policy, one producers could not follow, or one that drops
/// what its predecessor sealed without retiring it.
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

/// What [`handoff_policy_gate`] judged: the policy a cut seals into its bundle,
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

/// Refuse a policy whose rescues name no producer that can follow them, or bind
/// builds not cut yet
/// ([`PolicyFile::names_producers`](aterm_update_core::handoff_policy::PolicyFile::names_producers),
/// over [`builds_handing_over`] in `tree`'s ledger: no build at all when nothing
/// came before). Refuse one that drops a rescue `predecessor` sealed into its
/// bundle ([`PolicyFile::keeps`](aterm_update_core::handoff_policy::PolicyFile::keeps))
/// unless a `[[retired]]` entry of `text` names that rescue's exact range with a
/// reason and a date ([`Retired`](aterm_update_core::handoff_policy::Retired)), and
/// refuse an entry naming a rescue the predecessor sealed that this file keeps, so
/// the record never says a kept rescue was retired. An entry naming a range the
/// predecessor did not seal is the record of an earlier retirement, kept. The
/// sealed policy is the file at the commit ORIGIN's `v<release>` tag names
/// ([`shipped_commit`]), read the way a producer reads it, its top-level keys as
/// one rescue when it lists none; a tree without the file sealed none.
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

/// Every build `ledger_text` claimed up to `release`'s newest claim: the builds
/// whose producers can hand their sessions to the release that succeeds it. A
/// cut claims `max(last + 1, now)`, so it and every release after it are past
/// all of them, and a claim between `release`'s and the cut never shipped, or
/// it would be the predecessor ([`predecessor_release`]). Older abandoned claims
/// count too: the ledger says they were claimed, not whether one was installed.
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

/// The commit ORIGIN's `refs/tags/v<release>` names — the release that shipped,
/// where [`predecessor_release`] found it — made present here: fetched from
/// origin, into FETCH_HEAD alone, when this repository lacks it. Never
/// this clone's tag of that name: an abandoned cut deletes origin's tag and the
/// abandoning clone's only, a fetch never moves a tag a clone already holds, and
/// a recut re-points origin's, so a clone's tag can name a commit the release
/// that shipped was not cut from.
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

/// NO LIVE BUNDLE UNDER THE CUT'S `rm -rf`. Refuses — pre-claim, where a
/// refusal burns no build number — when any process is executing out of a cut
/// staging directory under `dist` (a per-claim `dist/cut-<build>.noindex/`,
/// [`crate::bundle::is_staging_dir_name`]), naming the
/// pids and the path; and when the process list cannot be read at all. Returns how
/// many processes it read.
///
/// WHY. The cut deletes and replaces staging bundles: `bundle::prepare_staging`
/// removes its own directory on a resume that signs again and prunes older ones. A bundle deleted under
/// a live process re-attributes that process to a path that no longer resolves, and
/// macOS answers a code identity it cannot build by REPLACING the stored requirement
/// — resetting the TCC grants of every copy of aterm on the Mac.
///
/// `lister` is injected so the three answers are tests: `Some([])` passes, a live
/// process refuses, and `None` — "could not look" — refuses too, because it is the
/// one answer that can never license a delete, and pre-claim is where refusing is
/// free. `bundle::prepare_staging` and `bundle::prune_staging_dirs` ask again at the
/// moment of deleting; this is the early refusal, not the only guard.
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
/// default `~/Developer/publication/mappings.json` (the engine checkout under the
/// developer root, `atpkg::devroot`) — the same location knob (a LOCATION, not a
/// skip switch) `ship release` reads. `pub publish` writes a row there
/// for every source publication: the dev commit it exported, keyed by its full sha.
/// A real cut releases the newest aterm row's commit ([`published_source`]).
#[must_use]
pub fn publication_mappings_path() -> PathBuf {
    env::var_os("PUBLICATION_ENGINE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .map(|home| atpkg::devroot::publication_engine(std::path::Path::new(&home)))
        })
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

/// THE CUT BUILDS THE PUBLISHED COMMIT (owner ruling R2), in the cut tree. Puts
/// [`cut_tree_path`] at `source`, detached, so everything the cut reads
/// and builds — Cargo.toml's version, the changelog's notes, the sources the proof
/// snapshot takes, the cutter itself — is exactly the commit `pub publish` exported
/// and the public source release carries.
///
/// WHY NOT MAIN'S TIP. The binary and the public source are one release under one
/// tag, so they must be one tree; a moving tip is one no gate passed. Building the
/// published commit makes peer pushes irrelevant to the cut: they neither block it
/// nor leak into it, and they ship in the next release.
///
/// WHY NOT THIS CHECKOUT. Several agent sessions share it: a detached HEAD there
/// parks their work on old code, and one that "fixes" it mid-cut fails the build
/// after the claim. The cut tree is the cutter's own; the operator's checkout never
/// moves, and nothing in it is read by the cut.
///
/// Preconditions it states itself: `origin` reachable (a real cut is never
/// offline), `source` present after the fetch, and `source` on `origin/main` —
/// `pub stage` exports main's history, and the release claim lands on main, so a
/// published commit main does not carry is a ledger that names another
/// repository's history.
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
/// BESIDE the checkout, never inside it: cargo reads `.cargo/config.toml` from its
/// working directory AND EVERY ANCESTOR, merging arrays such as `rustflags`, so a
/// tree under `dist/` or `target/` would build the published commit with this
/// checkout's configuration folded in — the leak the cut tree exists to close, and
/// on a flag-spelling change a build that dies at flag parse. `.noindex` keeps
/// Spotlight out of its release target trees, as it does for `dist/cut-*.noindex`.
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

/// Put the cut tree ([`cut_tree_path`]) at `commit`, detached — creating it as a
/// linked worktree of `repo`'s repository when it is absent. `git` runs in `repo`,
/// the operator's checkout, which this never changes.
///
/// A resume puts it back at its journaled release commit the same way, so "the tree
/// the build reads is the claim" is a placement, not an operator instruction.
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
        git_ok(
            git,
            &["worktree", "add", "-q", "--detach", &spelled, commit],
        )
        .map_err(|e| {
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
        let out = Command::new("git")
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
    // THE ONE CLEAN-TREE JUDGE ([`dirty_paths`]'s, which `clean_tree` asks of this tree
    // right after): never `git status --porcelain`, which also reads
    // `.git/info/exclude` and `core.excludesFile` and so passed what that judge then
    // refused. A submodule not populated is the alignment's to fix (below), not work.
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    let changes: Vec<String> =
        aterm_verify::identity::dirty_against(&path, &path_env, Some("HEAD"))
            .ok_or_else(|| {
                Error::new(format!(
                    "git could not list the state of the cut tree {}. Nothing was claimed.",
                    path.display()
                ))
            })?
            .into_iter()
            .filter(|(_, state)| *state != aterm_verify::identity::PathState::Unpopulated)
            .map(|(p, _)| p)
            .collect();
    if !changes.is_empty() {
        return Err(Error::new(format!(
            "the cut tree {} has changes — it belongs to the cutter, which never discards \
             work it did not make:\n  {}\nRemove it (`git worktree remove --force {}`), then \
             cut again; the next cut recreates it. Nothing was claimed.",
            path.display(),
            changes
                .iter()
                .take(5)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("\n  "),
            path.display()
        )));
    }
    let moved = rev_parse(&tree, "HEAD")? != commit;
    if moved {
        git_ok(&tree, &["checkout", "-q", "--detach", commit])?;
    }
    let path = checked_head(&path, commit)?;
    // Moved or not: a tree created before its commit carried a submodule, or one an
    // earlier failed alignment left behind, is brought to the gitlinks here too.
    align_submodules(&tree)?;
    Ok(CutTree { path, moved })
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

/// How far HEAD is from the newest commit a gate receipt vouches for — stated, never
/// required (see [`receipt_report`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptReport {
    /// The newest first-parent commit [`receipt_report`]'s predicate counts as gated, as
    /// `(short sha, subject)`; `None` when none was found within the scan.
    pub newest_gated: Option<(String, String)>,
    /// The first-parent commits above it, newest first, as `short subject`.
    pub ungated: Vec<String>,
    /// How many first-parent commits were scanned (the scan is bounded).
    pub scanned: usize,
    /// HEAD's own receipt verdict when it has one (`PASS` / `FAIL` / `COULD-NOT-RUN`):
    /// the receipt filed under HEAD, else the one filed under HEAD's tree.
    pub head_verdict: Option<String>,
    /// The receipt file [`Self::head_verdict`] was read from.
    pub head_receipt: Option<PathBuf>,
    /// When [`Self::newest_gated`] was gated BY ITS TREE — a passing receipt filed
    /// under the commit's tree by a run on another commit with the same bytes (a
    /// reworded amend, an identical-tree rebase) — that run's commit, short.
    pub gated_by_tree: Option<String>,
    /// When that tree receipt came from another clone's store ([`Stores`]),
    /// the store — the transcript says where the vouching receipt lives.
    pub gated_from: Option<PathBuf>,
    /// The reds [`Self::newest_gated`]'s receipt discharged the merge contract
    /// WITH: failures its run judged inherited from main (`inherited <id>` lines,
    /// the gate's differential verdict) — red on main with the same
    /// failure, so not that change's, and main's to fix. Empty for a commit gated
    /// by a clean merge, which has no receipt of its own.
    pub inherited: Vec<String>,
}

/// How many first-parent commits [`receipt_report`] reads before it stops looking.
pub const RECEIPT_SCAN_LIMIT: usize = 2000;

/// The first line of every gate receipt (`crates/aterm-verify/src/receipt.rs`
/// `MAGIC`); a file that does not start with it — an older format included — is
/// no receipt at all.
const RECEIPT_MAGIC: &str = "aterm-verify receipt 2";

/// The file-name prefix of a receipt filed under a TREE rather than a commit:
/// `<store>/tree-<tree sha>` (`crates/aterm-verify/src/receipt.rs`
/// `TREE_KEY_PREFIX`, which this mirrors — no dependency edge on the gate).
const RECEIPT_TREE_PREFIX: &str = "tree-";

/// `key value` from a receipt, `None` when absent or when the file is not a receipt.
fn receipt_field(text: &str, key: &str) -> Option<String> {
    let mut lines = text.lines();
    if lines.next() != Some(RECEIPT_MAGIC) {
        return None;
    }
    let prefix = format!("{key} ");
    lines
        .find_map(|line| line.strip_prefix(prefix.as_str()))
        .map(str::to_string)
}

/// Every `key value` line of a receipt with this key, in order — for the keys a
/// receipt repeats (`inherited`). Empty when the file is not a receipt.
fn receipt_fields(text: &str, key: &str) -> Vec<String> {
    let mut lines = text.lines();
    if lines.next() != Some(RECEIPT_MAGIC) {
        return Vec::new();
    }
    let prefix = format!("{key} ");
    lines
        .filter_map(|line| line.strip_prefix(prefix.as_str()))
        .map(str::to_string)
        .collect()
}

/// Where the gate keeps its receipts: `<git common dir>/aterm-verify/receipts`,
/// the one store every worktree of the repository shares
/// (`crates/aterm-verify/src/receipt.rs` `dir`).
pub(crate) fn receipt_store(git: &dyn GitRunner) -> Result<std::path::PathBuf> {
    let common = git_ok(
        git,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?
    .stdout_utf8();
    Ok(std::path::Path::new(common.trim()).join("aterm-verify/receipts"))
}

/// The git config key that names ANOTHER checkout of this repository whose
/// receipt store a cut also reads: `git config --add aterm-release.receiptsFrom
/// ~/aterm` in the cut clone. Multi-valued; read from this repository's own
/// config only (`--local`), so no global setting reaches every clone.
pub const RECEIPTS_FROM_KEY: &str = "aterm-release.receiptsFrom";

/// THE RECEIPT STORES A CUT READS.
///
/// The gate files its receipts per CLONE (`<git common dir>/aterm-verify/
/// receipts`), and the cut runs from a clone of its own, so without this a PASS or
/// a MEASURE run in the main checkout is invisible to it. `own` is this repository's store;
/// `others` are the stores of the checkouts [`RECEIPTS_FROM_KEY`] names — named,
/// never discovered (a cut clone's origin is the network, and a sibling found by
/// directory name could be any repository).
///
/// READ-ONLY, AND TREE-KEYED ONLY. Another store is only ever read — no file
/// written, no lock taken, nothing pruned — and only under keys that name the
/// bytes (`tree-<tree>`, a Trust facts key over the tree and prover), so what
/// counts there is exactly what would count here: every rule a receipt is judged
/// by is applied unchanged. (A MEASURE receipt is keyed by the executable the
/// release built in THIS clone, and read here alone: [`measured`].) Among the receipts under one key that
/// meet them, the newest run decides ([`Stores::newest_tree_keyed`]), and a
/// receipt from another store says which store it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stores {
    pub own: PathBuf,
    pub others: Vec<PathBuf>,
    /// A configured name that resolved to no store, with why — stated in what
    /// a lookup reports, never fatal.
    pub unresolved: Vec<String>,
}

impl Stores {
    /// This repository's store alone.
    #[must_use]
    pub fn own(own: PathBuf) -> Self {
        Self {
            own,
            others: Vec::new(),
            unresolved: Vec::new(),
        }
    }

    /// This repository's store, and every other one [`RECEIPTS_FROM_KEY`] names.
    ///
    /// # Errors
    /// git cannot name this repository's common dir, or read its config.
    pub fn of(git: &dyn GitRunner) -> Result<Self> {
        let own = receipt_store(git)?;
        let out = git.git(&[
            "config",
            "--local",
            "--type=path",
            "--get-all",
            RECEIPTS_FROM_KEY,
        ])?;
        let named = match out.status {
            0 => out.stdout_utf8(),
            1 => String::new(), // unset
            _ => {
                return Err(Error::new(format!(
                    "git config --get-all {RECEIPTS_FROM_KEY} failed: {}",
                    out.stderr_utf8().trim()
                )));
            }
        };
        let canonical = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let own_common = canonical(own.parent().and_then(Path::parent).unwrap_or(&own));
        let mut stores = Self::own(own);
        for name in named.lines().map(str::trim).filter(|n| !n.is_empty()) {
            let common = crate::ledger::GitCli::new(name)
                .git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
                .ok()
                .filter(|out| out.status == 0)
                .map(|out| canonical(Path::new(out.stdout_utf8().trim())));
            match common {
                None => stores.unresolved.push(format!(
                    "`{name}` ({RECEIPTS_FROM_KEY}) is not a git checkout, so no receipt there \
                     was read"
                )),
                Some(common) if common == own_common => {}
                Some(common) => {
                    let store = common.join("aterm-verify/receipts");
                    if !stores.others.contains(&store) {
                        stores.others.push(store);
                    }
                }
            }
        }
        Ok(stores)
    }

    /// Every store a TREE-keyed receipt may stand in: the own one first (`None`),
    /// then each other one (`Some(store)`, for saying where it came from).
    pub fn tree_keyed(&self) -> impl Iterator<Item = (&Path, Option<&Path>)> {
        std::iter::once((self.own.as_path(), None))
            .chain(self.others.iter().map(|s| (s.as_path(), Some(s.as_path()))))
    }

    /// THE NEWEST receipt filed under the tree key `key` across every store,
    /// among those `admit` takes (`text`, the store it came from): what one store
    /// would hold had every run filed there, since a later run's receipt replaces
    /// an earlier one's under the same key. Ordered by the receipt's `when` (a
    /// receipt without one is the oldest); on a tie the store read first — this
    /// clone's — keeps it. So a newer run that did NOT pass here is not outvoted
    /// by an older one that did elsewhere, nor the other way round.
    /// The others `admit` took are returned too, oldest last, for the transcript.
    fn newest_tree_keyed<'a>(
        &'a self,
        key: &str,
        mut admit: impl FnMut(&str, Option<&'a Path>) -> bool,
    ) -> Vec<(String, Option<&'a Path>)> {
        let mut found: Vec<(u64, String, Option<&'a Path>)> = Vec::new();
        for (dir, from) in self.tree_keyed() {
            let Ok(text) = fs::read_to_string(dir.join(key)) else {
                continue;
            };
            if admit(&text, from) {
                let when = receipt_field(&text, "when")
                    .and_then(|w| w.parse().ok())
                    .unwrap_or(0);
                found.push((when, text, from));
            }
        }
        // Stable: on equal `when` the store read first stays first.
        found.sort_by_key(|a| std::cmp::Reverse(a.0));
        found
            .into_iter()
            .map(|(_, text, from)| (text, from))
            .collect()
    }
}

/// A full pass: a receipt (the gate writes one only for a clean tree) that
/// discharged the merge contract ([`receipt_vouches`]). An unreadable file is
/// not a pass.
fn receipt_full_pass(receipts: &Path, sha: &str) -> bool {
    fs::read_to_string(receipts.join(sha)).is_ok_and(|text| receipt_vouches(&text))
}

/// Whether a receipt vouches for the bytes it was filed under: it discharged the
/// merge contract.
fn receipt_vouches(text: &str) -> bool {
    receipt_field(text, "merge-contract").as_deref() == Some("yes")
}

/// A full pass filed under a TREE ([`passing_trees`]): the commit its run was on,
/// and whether it passed WITH reds inherited from main (`inherited` lines).
#[derive(Debug, Clone, PartialEq, Eq)]
struct TreePass {
    head: String,
    inherited: bool,
    /// The other clone's store it was read from; `None` for the own store.
    store: Option<PathBuf>,
}

/// Every TREE a full pass vouches for, with the commit its run was on: the
/// `tree-<sha>` receipts that discharged the merge contract
/// ([`receipt_vouches`]). Read once per walk,
/// so a store with no tree receipts costs one directory read and no git call.
/// Every store in `stores` is read, and a tree's NEWEST receipt across them
/// decides ([`Stores::newest_tree_keyed`]): a later run that did not pass
/// takes the tree out, as it would have in one store.
fn passing_trees(stores: &Stores) -> std::collections::BTreeMap<String, TreePass> {
    let mut names = std::collections::BTreeSet::new();
    for (dir, _) in stores.tree_keyed() {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        names.extend(
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with(RECEIPT_TREE_PREFIX)),
        );
    }
    let mut trees = std::collections::BTreeMap::new();
    for name in names {
        let newest =
            stores.newest_tree_keyed(&name, |text, _| receipt_field(text, "head").is_some());
        let Some((text, from)) = newest.first() else {
            continue;
        };
        if receipt_vouches(text) {
            let pass = TreePass {
                head: receipt_field(text, "head").unwrap_or_default(),
                inherited: !receipt_fields(text, "inherited").is_empty(),
                store: from.map(Path::to_path_buf),
            };
            trees.insert(name[RECEIPT_TREE_PREFIX.len()..].to_string(), pass);
        }
    }
    trees
}

/// Does `pass` gate `commit`, whose tree it was filed under? A clean pass is a
/// statement about the bytes, so always. One that passed WITH inherited reds
/// was a DIFFERENTIAL verdict, judged against its run's merge-base
/// with main — it excused reds main had THERE — so it gates only a commit with
/// that same base: its own, or one with the same parents (a reworded amend).
/// An identical-tree rebase onto a main that fixed an inherited red, where the
/// conflict resolution put the red back, is not gated by it.
fn tree_pass_gates(git: &dyn GitRunner, pass: &TreePass, commit: &str) -> bool {
    if !pass.inherited || pass.head == commit {
        return true;
    }
    let parents = |sha: &str| {
        git.git(&["log", "-1", "--format=%P", sha])
            .ok()
            .filter(|out| out.status == 0)
            .map(|out| out.stdout_utf8().trim().to_string())
    };
    parents(commit).is_some_and(|mine| Some(mine) == parents(&pass.head))
}

/// THE UNGATED RANGE, STATED. Walks HEAD's first-parent history to the
/// newest commit a gate receipt vouches for — a passing receipt (merge contract
/// discharged), or a clean automatic merge of a receipted side (two parents, one
/// of them receipted, and the tree byte-equal to `git merge-tree --write-tree` of
/// the two) — and reports how many commits sit above it.
///
/// BY COMMIT, THEN BY TREE. A receipt is looked up under the commit, then under the
/// commit's tree (`tree-<sha>`): a reworded amend, a rebase or squash that lands on the
/// same tree, or a cherry-pick that reproduces it carries bytes a run already judged under
/// a new commit id. The report names the commit whose run vouched
/// ([`ReceiptReport::gated_by_tree`]), because the receipt is about the bytes — the few
/// gates that read history (the citation gate) ran on that commit.
///
/// GATED WITH MAIN'S REDS. A receipt's `merge-contract yes` means
/// nothing NEW failed: the gate judges a run against main's receipt for its base
/// and excuses a red main already had with the same failure, naming it in an
/// `inherited <id>` line. The report carries those names for the gated commit
/// ([`ReceiptReport::inherited`]) and the transcript states them, since a cut
/// from there ships them. Such a receipt gates another commit by its tree only
/// when that commit has the same base ([`tree_pass_gates`]).
///
/// THIS IS THE RECEIPT CHECK, AND IT IS INLINE. It reads the store `tools/verify.sh`
/// writes (`crates/aterm-verify/src/receipt.rs`) itself, in the cutter's preflight,
/// before the ledger claim; no git hook reads receipts (no hooks, no CI).
///
/// STATED, NOT REQUIRED. A receipt for the exact HEAD is a race the gate loses by
/// construction (it takes an hour, peers push every few minutes), so demanding one
/// would refuse every cut. What this buys is that the number is on the transcript. On a real
/// cut HEAD is the published commit, so the count is the published commit's. A
/// receipt on HEAD that did NOT pass is louder: the transcript warns
/// (`publish::ungated_range_lines`). What IS required on every cut is the L0
/// freeze-safety gate (`publish::run_freeze_safety_gate`), which runs itself rather
/// than trusting any record.
///
/// ACROSS CLONES. A tree receipt counts from any of `stores`
/// ([`Stores`]: this clone's, then the ones `aterm-release.receiptsFrom`
/// names, read-only), and the report names the store it came from
/// ([`ReceiptReport::gated_from`]). A commit-keyed receipt is read from this
/// clone's store alone.
///
/// # Errors
/// A git failure (a history that cannot be read is not an empty one).
pub fn receipt_report(git: &dyn GitRunner, stores: &Stores) -> Result<ReceiptReport> {
    let receipts = stores.own.as_path();
    // ONE git call for the whole walk — sha, parents and subject per commit — so a
    // checkout with no receipts at all costs one process, not one per commit.
    let limit = RECEIPT_SCAN_LIMIT.to_string();
    let walk = git_ok(
        git,
        &[
            "log",
            "--first-parent",
            "-n",
            &limit,
            "--format=%H%x1f%T%x1f%P%x1f%s",
            "HEAD",
        ],
    )?
    .stdout_utf8();
    let trees = passing_trees(stores);
    // A merge's side passes by its commit's receipt, or — only when the store holds
    // any tree receipt at all — by its tree's (one rev-parse, for a merge only). A
    // parent whose tree git cannot name (a shallow boundary) has no tree receipt:
    // the report then counts more commits ungated, never fewer.
    let side_passes = |parent: &str| {
        receipt_full_pass(receipts, parent)
            || (!trees.is_empty()
                && rev_parse(git, &format!("{parent}^{{tree}}")).is_ok_and(|tree| {
                    trees
                        .get(&tree)
                        .is_some_and(|pass| tree_pass_gates(git, pass, parent))
                }))
    };
    let mut above = Vec::new();
    let mut head_verdict = None;
    let mut head_receipt = None;
    let mut scanned = 0;
    for (index, line) in walk.lines().enumerate() {
        let mut fields = line.splitn(4, '\u{1f}');
        let (Some(sha), Some(tree), Some(parents), subject) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let subject = subject.unwrap_or_default();
        scanned += 1;
        if index == 0 {
            let tree_key = format!("{RECEIPT_TREE_PREFIX}{tree}");
            let found = fs::read_to_string(receipts.join(sha))
                .ok()
                .map(|text| (receipts.join(sha), text))
                .or_else(|| {
                    let newest = stores.newest_tree_keyed(&tree_key, |text, _| {
                        receipt_field(text, "verdict").is_some()
                    });
                    newest
                        .into_iter()
                        .next()
                        .map(|(text, from)| (from.unwrap_or(receipts).join(&tree_key), text))
                });
            head_verdict = found
                .as_ref()
                .and_then(|(_, text)| receipt_field(text, "verdict"));
            head_receipt = found
                .filter(|_| head_verdict.is_some())
                .map(|(path, _)| path);
        }
        let parents: Vec<&str> = parents.split_whitespace().collect();
        let by_commit = receipt_full_pass(receipts, sha);
        let by_tree = if by_commit {
            None
        } else {
            trees
                .get(tree)
                .filter(|pass| tree_pass_gates(git, pass, sha))
        };
        let gated = by_commit
            || by_tree.is_some()
            || (parents.len() == 2
                && (side_passes(parents[0]) || side_passes(parents[1]))
                && clean_automatic_merge(git, sha, parents[0], parents[1])?);
        if gated {
            // The receipt that gated it, when one did: what it inherited is named.
            let path = if by_commit {
                Some(receipts.join(sha))
            } else {
                by_tree.map(|pass| {
                    pass.store
                        .as_deref()
                        .unwrap_or(receipts)
                        .join(format!("{RECEIPT_TREE_PREFIX}{tree}"))
                })
            };
            let text = path.and_then(|path| fs::read_to_string(path).ok());
            let text = text.as_deref().unwrap_or_default();
            return Ok(ReceiptReport {
                newest_gated: Some((short(sha).to_string(), subject.to_string())),
                ungated: above,
                scanned,
                head_verdict,
                head_receipt,
                gated_by_tree: by_tree.map(|pass| short(&pass.head).to_string()),
                gated_from: by_tree.and_then(|pass| pass.store.clone()),
                inherited: receipt_fields(text, "inherited"),
            });
        }
        above.push(format!("{} {subject}", short(sha)));
    }
    Ok(ReceiptReport {
        newest_gated: None,
        ungated: above,
        scanned,
        head_verdict,
        head_receipt,
        gated_by_tree: None,
        gated_from: None,
        inherited: Vec::new(),
    })
}

/// Whether `merge`'s tree is git's own clean merge of its two parents — nothing
/// resolved or added by hand.
fn clean_automatic_merge(git: &dyn GitRunner, merge: &str, p1: &str, p2: &str) -> Result<bool> {
    let auto = git.git(&["merge-tree", "--write-tree", p1, p2])?;
    match auto.status {
        0 => {}
        1 => return Ok(false), // conflicts: not git's own clean result
        _ => {
            return Err(Error::new(format!(
                "git merge-tree --write-tree {p1} {p2} failed: {}",
                auto.stderr_utf8().trim()
            )));
        }
    }
    let auto_tree = auto.stdout_utf8().lines().next().unwrap_or("").to_string();
    let tree = rev_parse(git, &format!("{merge}^{{tree}}"))?;
    Ok(!auto_tree.is_empty() && auto_tree == tree)
}

fn short(sha: &str) -> &str {
    sha.get(..9).unwrap_or(sha)
}

/// THE PROVER A CUT JUDGES WITH — this process's pinned Trust toolchain — spelled as
/// the gate's receipts spell the one a run used (`crates/aterm-verify/src/lib.rs`
/// `write_receipt`'s `toolchain` line): `<stage2 bin dir> trustc <commit-hash>` — the
/// pinned stage2 ([`trust_stage2_bin`], canonical, as the gate's own resolution is)
/// and the `commit-hash:` its `trustc -vV` answers, `unknown` when it names none (as
/// the gate writes it). The cut's TRUST gate judges with it, and the cut signs only
/// bytes its `trustc` compiled (`publish::measured_release_bundle`): one compiler,
/// named by the bytes.
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
    let out = Command::new(&trustc).arg("-vV").output().map_err(|e| {
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

/// WERE THESE BYTES MEASURED, BY THE RELEASE'S OWN SUITE? (build once, 2026-10-09)
/// The one MEASURE judge — the release's measure step, the cut's sign step and `ship
/// check` all ask it: the receipt `tools/verify.sh --measure <app>` files under the
/// digest of the executable it judged (`aterm_verify::receipt::measure_key`), in
/// `store` (this clone's, `<git common dir>/aterm-verify/receipts`), says `measured
/// yes` about exactly `macho_sha256`, from a whole-tree run ON `judge` — the release's
/// R, the clean checkout whose measuring suites and thresholds (aterm-conformance's
/// paint and spin rows, `tools/paint-conformance/scan.py`) did the judging. The KEY
/// is the bytes, so no compiler, build environment or config line has to stand in for
/// them; the JUDGE is R's suite, so another worktree or commit of the clone — weaker
/// rows, older ones — cannot vouch for the bytes R ships.
///
/// `(true, the transcript line)`, or `(false, what the store holds instead)`. An
/// unreadable or foreign file is no measurement, never permission.
///
/// # Errors
/// The store cannot be read at all: no answer either way.
pub fn measured(store: &Path, macho_sha256: &str, judge: &str) -> Result<(bool, String)> {
    let key = aterm_verify::receipt::measure_key(macho_sha256);
    let shown = format!("`{}`", key.replace(macho_sha256, short(macho_sha256)));
    let text = match fs::read_to_string(store.join(&key)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((
                false,
                format!(
                    "no MEASURE receipt for the executable {} ({shown} is not in {})",
                    short(macho_sha256),
                    store.display()
                ),
            ));
        }
        Err(e) => {
            return Err(Error::new(format!(
                "read {}: {e}",
                store.join(&key).display()
            )));
        }
    };
    let Some(receipt) = aterm_verify::receipt::Receipt::parse(&text) else {
        return Ok((false, format!("{shown} is not a receipt this cutter reads")));
    };
    if receipt.artifact.as_deref() != Some(macho_sha256) {
        return Ok((false, format!("{shown} is about other bytes")));
    }
    if receipt.head != judge {
        return Ok((
            false,
            format!(
                "{shown} was judged by the measuring suite of {}, not of the release's {}",
                short(&receipt.head),
                short(judge)
            ),
        ));
    }
    if receipt.scope != "workspace" {
        return Ok((false, format!("{shown} is a narrowed run's")));
    }
    if receipt.measured != Some(true) {
        return Ok((
            false,
            format!(
                "{shown} says `measured {}` (verdict {})",
                match receipt.measured {
                    Some(false) => "no",
                    _ => "<absent>",
                },
                receipt.verdict
            ),
        ));
    }
    Ok((
        true,
        format!(
            "MEASURE tier: the executable {} MEASURED — `measured yes` in {shown}, judged by \
             {}'s suite",
            short(macho_sha256),
            short(&receipt.head)
        ),
    ))
}

/// `ship check`'s MEASURE line for `commit`: this clone's release journal's bundle
/// of it ([`crate::release::Journal::artifact`] — the journal lives in the git
/// common dir, so `publish/pre-promote`'s throwaway worktree reads it too) and the
/// answer [`measured`] gives for its executable. No bundle of `commit` built here
/// refuses: the source is never published ahead of a binary that cannot follow.
///
/// # Errors
/// Not measured, or not built yet, with the remedy; and a store or journal that
/// cannot be read.
pub fn measure_gate_at(git: &dyn GitRunner, commit: &str) -> Result<String> {
    let common = git_ok(
        git,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?
    .stdout_utf8();
    let common = Path::new(common.trim());
    let journal = crate::release::Journal::load(
        &common
            .join(crate::release::RELEASE_DIR)
            .join(crate::release::JOURNAL_FILE),
    )
    .map_err(Error::new)?;
    let Some(artifact) = journal
        .filter(|j| j.commit == commit)
        .and_then(|j| j.artifact)
    else {
        return Err(Error::new(format!(
            "no bundle of {} is built in this clone yet (its release journal names none) — \
             `{}` builds it once and MEASURE judges those bytes; nothing is signed unmeasured",
            short(commit),
            crate::release::RELEASE_COMMAND
        )));
    };
    match measured(&receipt_store(git)?, &artifact.macho_sha256, commit)? {
        (true, line) => Ok(line),
        (false, why) => Err(Error::new(format!(
            "the bundle of {} (build {}) is not MEASURED ({why}) — `{}` measures it: \
             `tools/verify.sh --measure {}`",
            short(commit),
            artifact.build,
            crate::release::RELEASE_COMMAND,
            artifact.app.display()
        ))),
    }
}

/// NOTHING SHIPS UNVERIFIED BY THE WHOLE-TREE TRUST LANE (owner ruling: the merge
/// contract verifies only the libraries a change touched, the release the whole
/// tree). Before the ledger claim, where a refusal burns no
/// build number, the tree this cut releases must be VERIFIED by the lane's facts
/// ([`crate::trust_facts::trust_report`]): a whole-tree run's measurements of
/// this tree — every byte but its floors, `tools/trust-gate-ratchet.tsv` and
/// `tools/trust-gate-memory.tsv` — with the prover `toolchain` names (the
/// [`cut_toolchain`] identity, whose `trustc` compiled the bytes the cut signs:
/// `publish::measured_release_bundle` refuses any other), judged NOW
/// against this commit's own floors (FACTS, NOT VERDICTS: a commit that changes
/// only the floors reuses the run before it, and its floors are
/// held to the merge contract's up-only rule against the run's). `ship release`
/// runs the whole-tree lane first (its step 5), which files them.
///
/// `refuse` is a real cut's; a dry run or rehearsal states the same answer as a
/// line — including that a real cut would refuse here — and goes on. Returns
/// the transcript line.
///
/// # Errors
/// A real cut of a tree the lane did not verify; a git failure.
pub fn trust_receipt_gate(git: &dyn GitRunner, refuse: bool, toolchain: &str) -> Result<String> {
    trust_receipt_gate_with(
        git,
        refuse,
        &crate::trust_facts::Prover::of_toolchain(toolchain),
        "HEAD",
    )
}

/// [`trust_receipt_gate`] for the prover `prover`, judging `rev` — HEAD for the cut,
/// the commit named for `ship check` ([`precheck`]).
fn trust_receipt_gate_with(
    git: &dyn GitRunner,
    refuse: bool,
    prover: &crate::trust_facts::Prover,
    rev: &str,
) -> Result<String> {
    use crate::trust_facts::{TrustReport, refusal, trust_report, verified_line};
    let head = rev_parse(git, rev)?;
    match trust_report(git, &Stores::of(git)?, prover, &head)? {
        report @ TrustReport::Verified { .. } => Ok(verified_line(&head, prover, &report)),
        // A NARROWED verdict's `why` names its one action; rerunning the lane
        // here only narrows again on the machine that ran it.
        TrustReport::Unverified { why, narrowed } if refuse => {
            Err(Error::new(refusal(&head, &why, narrowed)))
        }
        TrustReport::Unverified { why, narrowed } => {
            let files = if narrowed {
                ""
            } else {
                "; `tools/trust-gate-all.sh` on a clean checkout of it files the facts"
            };
            Ok(format!(
                "TRUST lane: {} NOT verified whole-tree ({why}) — a real cut would refuse \
                 here{files}",
                short(&head)
            ))
        }
    }
}

/// One gate [`precheck`] ran: its name, and the cut's own answer — the line
/// its transcript prints, or its refusal.
#[derive(Debug)]
pub struct PrecheckGate {
    pub name: &'static str,
    pub outcome: std::result::Result<String, String>,
}

/// What [`precheck`] found for one commit.
#[derive(Debug)]
pub struct Precheck {
    /// The commit judged, full.
    pub commit: String,
    /// Its `[workspace.package]` version: the release a cut of it would be.
    pub version: String,
    /// Every gate, in the cut's order — the cutter identity first, and each
    /// after it run whatever the one before said.
    pub gates: Vec<PrecheckGate>,
}

impl Precheck {
    /// The gates that would refuse a real cut of [`Self::commit`], in order.
    #[must_use]
    pub fn refused(&self) -> Vec<&'static str> {
        self.gates
            .iter()
            .filter(|gate| gate.outcome.is_err())
            .map(|gate| gate.name)
            .collect()
    }

    /// What `ship check` prints: a header, then one line per gate — `ok` with
    /// the cut's transcript line, or `REFUSE` with the cut's refusal and the
    /// remedy it names.
    #[must_use]
    pub fn transcript(&self) -> String {
        let mut out = format!(
            "aterm-release · check {} (v{}): the cut's pre-claim gates\n",
            short(&self.commit),
            self.version
        );
        for gate in &self.gates {
            match &gate.outcome {
                Ok(line) => out.push_str(&format!("  ok      {line}\n")),
                Err(why) => out.push_str(&format!(
                    "  REFUSE  {} — {}\n",
                    gate.name,
                    why.replace('\n', "\n          ")
                )),
            }
        }
        out
    }
}

/// THE SOURCE IS NEVER PUBLISHED AHEAD OF A BINARY THAT CANNOT FOLLOW IT: a
/// source release whose cut then refuses pre-claim is one no binary follows.
/// `ship check` runs, for one
/// commit and without a build of aterm, the pre-claim gates of a real cut that
/// read only the commit and this repository's receipt store, in the cut's
/// order:
///
/// * the cutter identity ([`own_cutter`]'s rule) FIRST — this binary is the
///   commit's own cutter: built from it. When it is not, the check REFUSES TO
///   ANSWER (an error, before any other gate): every answer after it would be
///   another tree's rules, which is how a check could say READY for a commit
///   whose own cutter then refuses;
/// * [`changelog_gate`] — the section a cut of it judges, `[Unreleased]` or
///   the version's own for a recut, with a real body and no `'''`
///   ([`changelog_gate_at`]);
/// * [`handoff_fixture_gate`] — the predecessor's handoff fixtures checked in
///   and pinned ([`handoff_fixture_gate_at`]);
/// * [`handoff_policy_gate`] — the handoff policy a cut of it seals into its
///   bundle, read strictly ([`handoff_policy_gate_at`]);
/// * [`measure_gate_at`] — this clone's bundle of it (its release journal) measured
///   ([`measured`]): the bytes a cut of it would sign;
/// * [`trust_receipt_gate`] — the whole-tree Trust lane's facts for its tree
///   (the floors aside), from that compiler's prover, passing its own floors.
///
/// Each reads the commit's objects, never a checkout, and is the cut's own
/// rule, so the answers and their words are the cut's; every gate after the
/// identity runs whatever the one before it said, so one check names
/// everything missing. `publish/pre-promote` builds the commit's own cutter
/// in a throwaway worktree of it and runs this for the commit `pub promote` is
/// about to publish, before public main moves.
///
/// It is not the cut's whole preflight: what needs a build (the fixture
/// guard's test run, the L0 freeze-safety gate), the tag and the published
/// channel (the tag and channel version gates) or the cut's own moment (the
/// lock, the roster, the credentials, the disk) is left to the cut, which runs
/// every gate again itself.
///
/// # Errors
/// `rev` names no commit; this binary is not that commit's cutter (with the
/// remedy); or the commit's Cargo.toml carries no release version.
pub fn precheck(git: &dyn GitRunner, rev: &str) -> Result<Precheck> {
    let toolchain = cut_toolchain().map_err(|e| {
        format!(
            "cannot name the prover this machine's cut judges with ({e}) — `aterm pkg \
             doctor` says why; TRUST_STAGE2_BIN pins one"
        )
    });
    precheck_with(git, rev, BUILD_COMMIT, toolchain)
}

/// [`precheck`] by a cutter stamped `stamp` ([`BUILD_COMMIT`]), for the cut
/// compiler `toolchain` — the [`cut_toolchain`] identity, or why there is none.
fn precheck_with(
    git: &dyn GitRunner,
    rev: &str,
    stamp: &str,
    toolchain: std::result::Result<String, String>,
) -> Result<Precheck> {
    let commit = rev_parse(git, &format!("{rev}^{{commit}}"))?;
    let identity = match own_cutter_with(git, stamp, &commit) {
        Ok(note) => note,
        Err(what) => {
            return Err(Error::new(format!(
                "ship check will not answer for {commit}: {what}. Its gates would be another \
                 tree's rules, not the ones a cut of that commit runs.\n\
                 fix:  ask with the commit's own cutter — `PUB_SOURCE_COMMIT={commit} \
                 publish/pre-promote` builds it in a throwaway worktree of the commit and runs \
                 the check there (what `pub publish` runs), or run `targo --unverified ship \
                 check` in a clean checkout of that commit"
            )));
        }
    };
    let manifest = git_ok(git, &["cat-file", "blob", &format!("{commit}:Cargo.toml")])?;
    let version = crate::publish::release_version_from_workspace(
        &crate::publish::workspace_version(&manifest.stdout_utf8())?,
    )?;
    let answer = |outcome: Result<String>| outcome.map_err(|e| e.to_string());
    let gates = vec![
        PrecheckGate {
            name: "cutter identity",
            outcome: Ok(identity),
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
            name: "MEASURE tier",
            outcome: answer(measure_gate_at(git, &commit)),
        },
        PrecheckGate {
            name: "TRUST lane",
            outcome: toolchain.and_then(|tc| {
                answer(trust_receipt_gate_with(
                    git,
                    true,
                    &crate::trust_facts::Prover::of_toolchain(&tc),
                    &commit,
                ))
            }),
        },
    ];
    Ok(Precheck {
        commit,
        version,
        gates,
    })
}

/// `targo --unverified ship check [--commit REV]`: [`precheck`] of `rev` in the
/// repository at `repo`, printed.
///
/// # Errors
/// Any gate that would refuse a real cut (every gate is printed first, each
/// refusal with its remedy), or [`precheck`]'s own — among them a checker that
/// is not the commit's own cutter, which answers nothing.
pub fn run_check(repo: &Path, rev: &str) -> Result<()> {
    let check = precheck(&crate::ledger::GitCli::new(repo), rev)?;
    print!("{}", check.transcript());
    let refused = check.refused();
    if refused.is_empty() {
        println!(
            "READY — a real cut of {} (v{}) passes these gates; the cut runs them, and \
             every other gate, again itself",
            short(&check.commit),
            check.version
        );
        return Ok(());
    }
    Err(Error::new(format!(
        "a real cut of {} (v{}) would refuse at: {}; do not publish its source until it passes",
        short(&check.commit),
        check.version,
        refused.join(", ")
    )))
}

/// The transcript line for what [`handoff_fixture_gate`] found: the cut's, and
/// `ship check`'s.
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
///
/// The pure comparison is [`channel::check_channel_version`]; this is only its I/O
/// shell — resolve the channel slug from the local manifest, read that channel's
/// `Cargo.toml` at `main`, hand both to the pure function.
///
/// `Ok(None)` when there is nothing to check: no `update_channel` configured, or
/// the channel carries no workspace manifest. `Ok(Some(version))` on agreement.
///
/// Fetch failures fail CLOSED. An unreachable channel is indistinguishable from a
/// channel that disagrees, and the whole purpose of the gate is to stop guessing
/// about the channel's contents. `offline` (from `--dry-run` / `--rehearse`, never from
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
        // A 404 is AMBIGUOUS and must not be trusted as "no manifest". GitHub also
        // answers 404 for a repository the credential cannot read, precisely so it
        // leaks nothing — so a missing or wrong channel token would otherwise make
        // this gate silently skip itself, which is the opposite of failing closed.
        // Distinguish the two by asking whether the REPO is readable at all.
        let probe = channel_api(&format!("repos/{slug}"))?;
        if !probe.status.success() {
            return Err(Error::new(format!(
                "the public channel {slug} is not readable with the available credential, so \
                 this cut cannot confirm it carries v{version} (GitHub's 404 means missing \
                 or unauthorized).\n\
                 fix:  provide ~/.secrets/gh_access_token_alabsystems, or cut with --dry-run \
                 or --rehearse"
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

/// One `gh api` call against the public channel, credentialed for the release org.
///
/// The channel is a DIFFERENT org than the dev remote, and `gh auth`'s account is
/// the dev one, which cannot read it. The token goes in the environment, never in
/// argv, so it cannot surface in a process listing or a transcript.
fn channel_api(path: &str) -> Result<std::process::Output> {
    let mut command = Command::new("gh");
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

/// Does this `gh` stderr report a 404? `gh` prints `gh: Not Found (HTTP 404)` plus
/// the JSON body, so either spelling is enough — and both are matched because the
/// caller does NOT act on the answer alone (see the repo probe above).
fn is_not_found(stderr: &str) -> bool {
    stderr.contains("404") || stderr.contains("Not Found")
}

/// Prove the committed Cargo.lock already resolves the workspace without any
/// rewrite or network access. App cuts deliberately preserve the independent
/// source version and lockfile, so a stale lock must fail before the ledger
/// claim rather than dirtying the tree during the expensive build.
pub fn locked_metadata_gate(repo: &Path) -> Result<()> {
    let lock_path = repo.join("Cargo.lock");
    let lock_before = fs::read(&lock_path).map_err(|error| {
        Error::new(format!(
            "read committed Cargo.lock before release metadata check: {error}"
        ))
    })?;
    let targo = resolve_targo()?;
    let out = Command::new(&targo)
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

/// Releases are cut ONLY on the owner's arm64 Mac (spec §6). Compile-time cfg
/// IS the host check here: the ship binary is always built on the cutting
/// machine via the `targo --unverified ship` run alias — never cross-compiled, never
/// `cargo install`ed (spec decision 13) — so target == host by construction.
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

/// Clean tree: the release commit must be EXACTLY the changelog-roll + ledger
/// changes, and a rejected-push retry does `reset --hard` — stray local edits
/// would be destroyed, so refuse them up front.
///
/// THE ONE CLEAN-TREE JUDGE: [`aterm_verify::identity::dirty_against`] against `HEAD`,
/// the Trust lane's own rule (the tracked `.gitignore` files, never
/// `.git/info/exclude` or `core.excludesFile`), which `ship release` judges its clone
/// with too — so no step of a release can call a tree clean that another calls dirty.
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

/// The paths [`aterm_verify::identity::dirty_against`] finds dirty in the checkout at
/// `root` against its `HEAD` — the one judge [`clean_tree`] and `ship release` share.
///
/// # Errors
/// git could not list the checkout's state.
pub fn dirty_paths(root: &Path) -> Result<Vec<String>> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    aterm_verify::identity::dirty_against(root, &path, Some("HEAD"))
        .map(|dirty| dirty.into_keys().collect())
        .ok_or_else(|| {
            Error::new(format!(
                "git could not list the state of {}",
                root.display()
            ))
        })
}

/// Every submodule initialised and checked out at exactly the commit `HEAD`
/// records for it (`aterm-link` path-depends on crates inside `vendor/astream`).
///
/// `clean_tree` cannot see the worst of it: an UNINITIALISED submodule is not
/// dirty to `git status`, and cargo then stops at `failed to get astream-broker
/// as a dependency of aterm-link` minutes into the build. A submodule on
/// another commit IS dirty to `git status`, but "commit or remove them" is the
/// wrong remedy for the common cause — a `git pull` that moved the gitlink and
/// left the checkout where it was. So this names the submodule and the fix.
/// Read-only: `git submodule status` changes nothing.
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

/// After [`place_cut_tree`] creates or moves the cut tree: every submodule to the
/// commit the tree's NEW `HEAD` records, from its already-configured remote when the
/// commit is not local yet.
///
/// A linked worktree does not bring a submodule's checkout with it, and `checkout
/// --detach` moves the gitlink and never the checkout — so without this the published
/// commit would be built against no astream at all, or against the astream of the
/// tree's previous commit. The release operator reads aterm's private remote, so
/// reading astream's is the same credential. (A claim needs no such step:
/// `ledger::claim` leaves the cut tree at the published commit until it lands, then
/// moves it to the release commit, whose gitlinks are the published commit's.) Fails
/// loudly, and leaves nothing half-done that the refusal does not name.
pub fn align_submodules(git: &dyn GitRunner) -> Result<()> {
    let out = git.git(&["submodule", "update", "--init", "--recursive", "--checkout"])?;
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

/// A real 40-hex commit id — not `"unknown"`, not git's reserved all-zero null
/// object id that `build.rs` stamps on a dirty source closure.
pub fn canonical_commit(value: &str) -> bool {
    aterm_codec::hex::is_lower(value, 40) && value != DIRTY_BUILD_COMMIT
}

/// The commit THIS BINARY was built from, stamped by `build.rs`; the reserved
/// null object ID means its repository source closure was dirty, and
/// `"unknown"` means the build could not establish either fact.
pub const BUILD_COMMIT: &str = env!("ATERM_RELEASE_BUILD_COMMIT");
const DIRTY_BUILD_COMMIT: &str = "0000000000000000000000000000000000000000";

/// What a claim commit changes on top of the commit it claims: its ledger line and
/// its changelog roll (`ledger::reserve` makes it, `ledger::claim` lands it), and
/// nothing else.
pub const CLAIM_FILES: [&str; 2] = [crate::changelog::CHANGELOG_FILE, crate::ledger::LEDGER_FILE];

/// THE CUTTER IS ITS COMMIT'S OWN. Every other pre-claim gate proves something about
/// the TREE and nothing about the binary doing the proving: a cutter built from an
/// older tree validates its output against its OWN older rules, and passes on a clean
/// tree (v0.63.0 shipped a seeded image that way). So the binary carries the commit
/// it was built from ([`BUILD_COMMIT`]), and it acts for `commit` only when it IS that
/// commit — or `commit` is the claim commit made on top of it: its first parent the
/// stamp, and nothing but [`CLAIM_FILES`] differing, the one shape `ledger::reserve`
/// makes. No source closure is reasoned about: `targo --unverified ship release`
/// re-runs itself as the release commit's own cutter before it judges anything
/// (`release.rs`), and the cut it runs is that same binary.
///
/// # Errors
/// This binary is not `commit`'s cutter, and why.
pub fn own_cutter(git: &dyn GitRunner, commit: &str) -> std::result::Result<String, String> {
    own_cutter_with(git, BUILD_COMMIT, commit)
}

/// [`own_cutter`] for a cutter stamped `stamp` — pure over the stamp, so every case is
/// a test rather than a rebuild.
///
/// # Errors
/// As [`own_cutter`].
pub fn own_cutter_with(
    git: &dyn GitRunner,
    stamp: &str,
    commit: &str,
) -> std::result::Result<String, String> {
    if stamp == "unknown" || stamp.is_empty() {
        return Err(
            "this aterm-release binary carries no build commit (built without git, or from \
             an export)"
                .to_string(),
        );
    }
    if stamp == DIRTY_BUILD_COMMIT {
        return Err(
            "this aterm-release binary was built from a dirty repository source closure"
                .to_string(),
        );
    }
    if !canonical_commit(stamp) {
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
/// because its results came from other code than the tree's. A rehearsal mutates its
/// scratch repository, so it gets no exemption.
///
/// # Errors
/// This binary is not `commit`'s cutter, with the remedy.
pub fn own_cutter_gate(git: &dyn GitRunner, commit: &str, allow_stale: bool) -> Result<()> {
    match own_cutter(git, commit) {
        Ok(_) => Ok(()),
        Err(what) if allow_stale => {
            println!(
                "==> cutter identity: {what} — allowed because this dry-run publishes nowhere, \
                 but it is running OTHER code than the tree"
            );
            Ok(())
        }
        Err(what) => Err(Error::new(own_cutter_refusal(&what, commit))),
    }
}

/// The refusal [`own_cutter_gate`] says: what is wrong, then the one way to run as the
/// commit's own cutter.
fn own_cutter_refusal(what: &str, commit: &str) -> String {
    format!(
        "{what}.\n\
         fix:  run it as `{}` from the release's clone (it re-runs itself as the release \
         commit's own cutter), or rebuild this cutter at {commit}: `targo clean --release -p \
         aterm-release` (`--release` required: the `ship` alias is `run --release`; `clean` \
         takes no lane flag)",
        crate::release::RELEASE_COMMAND
    )
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

/// Tag vX.Y.Z must be absent BOTH locally and on origin: the publish step mints
/// it late (spec decision 5), so any pre-existing tag means this version was
/// already cut (or half-cut) — colliding with it would re-point a published
/// artifact. Local and remote are checked separately because either alone can
/// be stale — and which one holds it decides the remedy: a tag on origin is a cut
/// (the cutter's own `tag` step pushed it), so the next release is the next MINOR,
/// published first; a tag only here is a leftover (an abandoned cut deletes its tag
/// on origin), and this version can still be cut once it is gone.
pub fn tag_free(git: &dyn GitRunner, version: &str) -> Result<()> {
    let tag = format!("v{version}");
    // Local: `rev-parse -q --verify` exits 0 iff the ref EXISTS — existence is
    // the failure here, so this is the one git call whose non-zero exit is the
    // happy path.
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

/// Changelog gates (spec §3), delegated to changelog.rs: the section's real
/// body non-empty, no `'''`. `section` is "Unreleased" for a fresh cut, the
/// version itself for a recut (see [`GateOpts::recut`]).
pub fn changelog_gate(repo: &Path, section: &str) -> Result<changelog::GateSummary> {
    changelog_section_gate(
        &GateTree::Dir(repo).text(changelog::CHANGELOG_FILE)?,
        section,
    )
}

/// [`changelog_gate`] for a COMMIT, read from its objects, judging the section a
/// cut of it judges — `run_cut`'s `recut`: the version's own section when the
/// commit already carries one, else `[Unreleased]`. Returns that section with
/// what the gate found.
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

/// The one rule [`changelog_gate`] and [`changelog_gate_at`] apply to a
/// changelog's text.
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

/// What [`handoff_fixture_gate`] found for the release this cut succeeds.
#[derive(Debug, PartialEq, Eq)]
pub struct HandoffFixtures {
    /// The release, as the ledger records it (`0.92.0`).
    pub release: String,
    /// Its desk directories that hold a `parent.toml`, sorted.
    pub desks: Vec<String>,
    /// Its desks named in the guard's `PINNED_DESKS`, in table order.
    pub pinned: Vec<String>,
}

/// Refuse a cut whose predecessor's handoff fixtures are not checked in and
/// pinned.
///
/// An installed release N updates to N+1 by handing its sessions to the new
/// build, so that hop is decided by N's frozen producer and N+1's consumer. The
/// only thing that checks N+1's consumer against what N really writes is N's
/// fixture directory, run by the guard in [`HANDOFF_FIXTURE_GUARD`]. A missing one
/// is invisible: every handoff test for that hop then writes and reads with the
/// same build, and stays green while the real hop fails. Refusing here, before the
/// claim, makes a missing directory cost seconds.
///
/// The predecessor is [`predecessor_release`] over `RELEASES.ledger`, with
/// "shipped" meaning its `v<version>` tag is on origin. The cutter mints that
/// tag when it publishes and an abandoned cut deletes it, so a claim that was
/// abandoned and then skipped (docs/RELEASING.md, "Resume, recut, abandon") is
/// passed over instead of demanding fixtures from a tag that does not exist.
/// The version being cut is never its own predecessor, which is all a recut
/// needs: it re-claims that version, and still succeeds the same release.
///
/// Runs for every flavour, so `--dry-run` shows the refusal a real cut would
/// meet. `Ok(None)` only when the ledger records no other version at all.
pub fn handoff_fixture_gate(
    git: &dyn GitRunner,
    repo: &Path,
    version: &str,
) -> Result<Option<HandoffFixtures>> {
    handoff_fixture_gate_in(git, &GateTree::Dir(repo), version)
}

/// [`handoff_fixture_gate`] for a COMMIT rather than a checkout: the ledger,
/// the fixture directories and the guard are read from `commit`'s objects, so
/// `ship check` ([`precheck`]) judges exactly the tree a cut of it would build,
/// whatever this checkout holds. `git` is the repository's; origin's tags are
/// listed through it as the cut lists them.
pub fn handoff_fixture_gate_at(
    git: &dyn GitRunner,
    commit: &str,
    version: &str,
) -> Result<Option<HandoffFixtures>> {
    handoff_fixture_gate_in(git, &GateTree::Commit { git, commit }, version)
}

/// Where a pre-claim gate reads the tree it judges: the cut tree on disk, which
/// `clean_tree` has already proved equal to its HEAD, or a commit's objects
/// (`ship check`, [`precheck`]). The handoff-fixture and changelog gates read
/// through it, so the cut and the check judge the same bytes by one rule.
pub(crate) enum GateTree<'a> {
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

/// [`handoff_fixture_gate`] over either kind of tree.
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
    // ONE listing, not a probe per version: walking back past an abandoned
    // claim must not cost a network round trip per step.
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

/// Judge one release's fixtures: its directory exists, at least one desk in it
/// holds a `parent.toml`, and the guard pins at least one desk of it. Those are
/// the two halves the guard needs — desks it can discover and run, and a pinned
/// row, without which losing the directory again would leave the guard green
/// while it checks less. `tree` is the cut tree on disk or a commit's objects.
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
    // THE DESK SET. One pinned desk is not enough: a release whose generator
    // wrote only `history` would leave most of its producer unchecked. Each
    // desk the guard requires must be pinned and on disk for every release from
    // ITS OWN floor on — not one floor for the whole set, since a desk added for
    // release N can never appear in a frozen release before it.
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

/// The libtest filter [`handoff_fixture_guard_gate`] runs: the module of the
/// guard in [`HANDOFF_FIXTURE_GUARD`] (`seamless.rs` mounts it as
/// `fixture_tests`).
pub const HANDOFF_FIXTURE_GUARD_FILTER: &str = "seamless::fixture_tests::";

/// The guard's own target directory in the cut tree: a root sibling the
/// checked-in `.gitignore` names (`/target-*`), so it never dirties the tree
/// the cut asserts clean, and apart from `target/` (which may be the
/// `target.noindex` link the release lane validates) and from the release
/// build's products.
pub const HANDOFF_FIXTURE_GUARD_TARGET: &str = "target-fixture-guard";

/// RUN THE CROSS-VERSION GUARD IN THE CUT TREE, before the claim.
///
/// [`handoff_fixture_gate`] proves the predecessor's fixtures are checked in
/// and pinned. It cannot prove this build's consumer adopts them: that is
/// what the guard in [`HANDOFF_FIXTURE_GUARD`] runs. A consumer change that
/// refused a shipped producer's bytes would otherwise ship, and every installed
/// copy of that producer would stay on it. So the cut runs it, in the tree it
/// is about to build: `targo --unverified test -p aterm-gui --lib` filtered to
/// [`HANDOFF_FIXTURE_GUARD_FILTER`].
///
/// A red guard refuses, with the failing tests named. So does a run in which
/// NO test ran: a filter that matched nothing (a renamed module) is a guard
/// that checked nothing, never a pass.
///
/// Its target directory is [`HANDOFF_FIXTURE_GUARD_TARGET`], made afresh and
/// removed after the run whatever its outcome: a few GiB the cut does not keep,
/// and a `deps/` holding one test binary (AGENTS.md, "Keep `target/debug/deps`
/// small"). These tests open PTYs and never reach WindowServer. The build's
/// steering variables (`RUSTFLAGS`, `RUSTC`, …) are scrubbed, as for the
/// release build: an inherited `RUSTFLAGS` replaces the config's rustflags and
/// would turn Trust verification on for every unit.
///
/// `run` is the process runner, so a test can hand it any outcome.
pub fn handoff_fixture_guard_gate(
    tree: &Path,
    run: &mut dyn FnMut(&mut Command) -> std::io::Result<std::process::Output>,
) -> Result<usize> {
    handoff_fixture_guard_with(&resolve_targo()?, tree, run)
}

/// [`handoff_fixture_guard_gate`] with the `targo` it runs named: a test needs no
/// toolchain, and `ship release` runs it with its one prover's.
pub(crate) fn handoff_fixture_guard_with(
    targo: &Path,
    tree: &Path,
    run: &mut dyn FnMut(&mut Command) -> std::io::Result<std::process::Output>,
) -> Result<usize> {
    let target = tree.join(HANDOFF_FIXTURE_GUARD_TARGET);
    clear_path(&target).map_err(|e| {
        Error::new(format!(
            "handoff fixture guard: cannot clear its target directory {}: {e}",
            target.display()
        ))
    })?;
    println!(
        "==> handoff fixture guard: every shipped producer's desks through this build's \
         consumer (targo --unverified test -p aterm-gui --lib -- {HANDOFF_FIXTURE_GUARD_FILTER})"
    );
    let mut command = aterm_gui_lib_test(targo, tree, &target, &[HANDOFF_FIXTURE_GUARD_FILTER]);
    let outcome = run(&mut command);
    // Removed whatever the outcome; a failure to remove is said, never a refusal
    // of its own — the verdict is the guard's.
    if let Err(error) = clear_path(&target) {
        println!(
            "==> handoff fixture guard: could not remove {} ({error}); remove it by hand",
            target.display()
        );
    }
    let output = outcome.map_err(|e| {
        Error::new(format!(
            "handoff fixture guard: cannot run {}: {e}",
            targo.display()
        ))
    })?;
    fixture_guard_verdict(
        output.status.success(),
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
    )
}

/// `targo --unverified test -p aterm-gui --lib --target-dir <target> -- <args>` in
/// `tree`, with the build's steering variables (`RUSTFLAGS`, `RUSTC`, …) removed, as
/// for the release build: an inherited `RUSTFLAGS` replaces the config's rustflags
/// and would turn Trust verification on for every unit. The fixture guard runs it,
/// and so does the release's own fixture generator (`crate::fixtures`).
pub(crate) fn aterm_gui_lib_test(
    targo: &Path,
    tree: &Path,
    target: &Path,
    args: &[&str],
) -> Command {
    let mut command = Command::new(targo);
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
        .stdin(std::process::Stdio::null());
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

/// Read the guard's libtest output: how many tests passed, or the refusal —
/// for a failed run (naming the failing tests, or the build's last error
/// lines when it never ran a test) and for a run in which no test ran.
fn fixture_guard_verdict(success: bool, stdout: &str, stderr: &str) -> Result<usize> {
    let (passed, failed, failing) = libtest_tally(stdout);
    let refuse = |why: String| {
        Error::new(format!(
            "handoff fixture guard: {why}\n\
             until it passes, nothing proves an installed release can hand its sessions to \
             this build. Nothing was claimed.\n\
             fix:  run `targo --unverified test -p aterm-gui --lib -- \
             {HANDOFF_FIXTURE_GUARD_FILTER}` and fix the CONSUMER, never a fixture \
             ({HANDOFF_FIXTURE_README})"
        ))
    };
    if !success || failed > 0 {
        let why = if failing.is_empty() {
            format!(
                "the guard did not pass, and no test failed by name — the build or the run \
                 stopped first:\n{}",
                build_error_lines(stderr)
            )
        } else {
            format!("{} test(s) failed: {}", failing.len(), failing.join(", "))
        };
        return Err(refuse(why));
    }
    if passed == 0 {
        return Err(refuse(format!(
            "no test ran — the filter `{HANDOFF_FIXTURE_GUARD_FILTER}` matched nothing, so the \
             guard checked nothing (was its module renamed?)"
        )));
    }
    Ok(passed)
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

/// `gh auth status` must succeed — the publish half of the cut talks to GitHub,
/// and discovering a dead token AFTER the build wastes ten minutes. Returns
/// the account name when parseable (transcript garnish; never load-bearing).
pub fn gh_auth() -> Result<Option<String>> {
    let out = Command::new("gh")
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
    // gh has historically split this output between stdout and stderr; scan
    // both for "… account <name> …".
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

/// The `[target.…]` tables in `.cargo/config.toml` that can carry the native
/// Trust lane's rustflags, in lookup order. The one table is not a triple:
/// `[target.'cfg(trust_verify)']` is scoped to the COMPILER that understands the
/// flag rather than to a host, so it reaches every Trust lane on every target.
/// `the_gate_knows_the_name_of_the_table_the_config_actually_carries` goes red
/// the day the table moves.
const TRUST_LANE_TABLES: [&str; 1] = ["cfg(trust_verify)"];

/// trustc's own words for "the off-switch took effect".
///
/// The gate reads THIS rather than matching a flag spelling, and the distinction
/// is the point: hardcoding `-Ztrust-verify=off` anywhere in the cutter is the
/// drift [`native_lane_rustflags`] exists to avoid, so the gate asks the compiler
/// whether the native lane — `targo --unverified` under the config's flags,
/// whatever either spells — actually turned verification off. The switch is the
/// LANE's, not the config's: `targo trust` refuses a project-config
/// `-Ztrust-verify=off`, so the config carries none and `targo --unverified`
/// appends its own to every unit (docs/ISSUE-trust-unlaned-offswitch-2026-10-07.md).
/// Measured against [`PROBE_SRC`]: the off lane prints `note: compiled WITHOUT
/// Trust verification — 0 obligations checked`; a verifying one prints a
/// `Trust Verification Report` and `1 proved, … out of 1 obligation(s)` instead.
const OFF_SWITCH_NOTE: &str = "compiled WITHOUT Trust verification";

/// The probe's source. It carries ONE real Level 0 obligation on purpose — the
/// divisor-is-zero check on `numerator / denominator`.
///
/// A program with no obligations prints "0 obligations checked" in both lanes,
/// so the probe could not tell them apart; one obligation makes the two lanes
/// print different things, which is what [`verification_off_verdict`] reads.
const PROBE_SRC: &str = "\
pub fn divide(numerator: u32, denominator: u32) -> u32 {
    numerator / denominator
}

fn main() {
    let _ = divide(6, 3);
}
";

/// The native-lane rustflags the repo's `.cargo/config.toml` applies to the
/// Trust slice (`--cfg clean_islands` and the front-end thread count; the
/// verification opt-out is the `targo --unverified` lane's own). Read from the
/// file, never hardcoded: a hardcoded spelling drifts from the config, and either
/// direction of that drift kills the cut in the build step.
///
/// FAILS CLOSED when the file is there and none of [`TRUST_LANE_TABLES`] is:
/// through a single dead key a RENAMED table is indistinguishable from a deleted
/// one, and an empty answer would let the probe run flagless and gate on nothing.
fn native_lane_rustflags(repo: &Path) -> Result<Vec<String>> {
    let path = repo.join(".cargo/config.toml");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        // No config at all is not drift: there is no table to have been renamed
        // and nothing applies flags to anything. The empty list still refuses,
        // one step later in `verification_off_verdict`, where it reads well.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(Error::new(format!("read {}: {error}", path.display())));
        }
    };
    let value: aterm_toml::Value = text
        .parse()
        .map_err(|error| Error::new(format!("parse {}: {error}", path.display())))?;
    trust_lane_rustflags(&value).ok_or_else(|| {
        Error::new(format!(
            "{} carries no Trust-lane rustflags: none of {TRUST_LANE_TABLES:?} under \
             [target] has a `rustflags` array, so the probe would run flagless.\n\
             fix:  if the table moved, add its name to TRUST_LANE_TABLES",
            path.display()
        ))
    })
}

/// The pure half of [`native_lane_rustflags`]: the first of
/// [`TRUST_LANE_TABLES`] that carries a `rustflags` array, as strings.
///
/// Split out so the table-name contract is a machine-checked test against the
/// config this repo actually ships — on every host, with no Mac and no Trust
/// toolchain in sight — instead of a comment. The drift it guards is invisible
/// from the machine whose lane it breaks.
fn trust_lane_rustflags(config: &aterm_toml::Value) -> Option<Vec<String>> {
    let targets = config.get("target")?;
    TRUST_LANE_TABLES.iter().find_map(|name| {
        Some(
            targets
                .get(*name)?
                .get("rustflags")?
                .as_array()?
                .iter()
                .filter_map(|flag| flag.as_str().map(String::from))
                .collect(),
        )
    })
}

/// Did the native lane (`targo --unverified` under the config's flags) actually
/// turn verification off in the trustc that just ran? Pure over (flags,
/// trustc's stderr), so the verdict is a test rather than a promise.
///
/// Two drifts die here, all of them at the cheap pre-claim moment instead of
/// twenty minutes into the build step:
///
/// * the flag list arrived EMPTY — the Trust-lane table renamed away again, or a
///   `.cargo/config.toml` that is not this repo's;
/// * the lane did not switch: a `targo` that is not the real targo (a stale
///   stage shipping upstream cargo under that name), or a lane that stopped
///   appending its off-switch — trustc prints its verification report instead
///   of [`OFF_SWITCH_NOTE`].
///
/// A spelling the compiler cannot parse never reaches here — that one fails the
/// compile itself, one branch above this call.
fn verification_off_verdict(flags: &[String], stderr: &str) -> Result<()> {
    if flags.is_empty() {
        return Err(Error::new(
            "the native lane's rustflags came back EMPTY, so the trustc probe gated on \
             nothing (.cargo/config.toml's Trust-lane table supplies them)",
        ));
    }
    if !stderr.contains(OFF_SWITCH_NOTE) {
        return Err(Error::new(format!(
            "the probe compiled through `targo --unverified` under rustflags {flags:?} \
             and trustc did NOT report \"{OFF_SWITCH_NOTE}\": the lane did not switch \
             verification off, so the real build would verify every unit and fail. \
             trustc said: {}\n\
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

/// Probe the trustc the native slice uses — the Trust stage2 compiler that
/// `targo` drives (spec §6 buildplan.rs). Always on: the repo compiles with
/// Trust, so a missing or broken toolchain is a broken toolchain, never a
/// fallback. The exact path is printed on failure so the remediation is
/// copy-pasteable.
///
/// The probe COMPILES [`PROBE_SRC`] through the native lane itself — `targo --unverified
/// check` in a scratch package carrying a copy of the repo's .cargo/config.toml — not just
/// `--version`: a stage2 whose library build never landed has a runnable trustc but no std
/// rlibs in its sysroot (every crate E0463s in the real build), and an off-switch spelling
/// this trustc does not parse fails every unit the same way. Compiling is the only honest
/// check that the toolchain can do what the build lane is about to ask of it; `check` keeps
/// it fast (metadata only, no codegen, no link).
///
/// Then it reads what trustc SAID, through [`verification_off_verdict`]. A
/// compile that succeeds proves the toolchain works; only the compiler's own
/// note proves the lane's opt-out reached it. An exit status alone cannot,
/// which is why [`PROBE_SRC`] carries an obligation: the two lanes have to
/// differ before there is anything for a gate to read. The probe goes through
/// the same driver and lane as the build: the config carries no off-switch, so a
/// direct trustc call would verify.
pub fn trustc_probe(repo: &Path) -> Result<PathBuf> {
    let trustc = trust_stage2_bin()?.join("trustc");
    let probe = Command::new(&trustc).arg("--version").output();
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
    // `--unverified`, under a copy of the repo's config (so the Trust-lane
    // table's flags apply exactly as in the build), in a scratch package.
    let flags = native_lane_rustflags(repo)?;
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
    // The build lane scrubs inherited compiler steering (buildplan `run_cargo`);
    // so does the probe, or a stale RUSTFLAGS would replace the config's flags.
    // Its own CARGO_HOME, as the build it stands in for pins one (buildplan's
    // `take.cargo_home`): a user-level `~/.cargo/config.toml` must not steer
    // the probe where it cannot steer the build. The package has no deps and
    // runs `--offline`, so an empty home serves.
    let out = Command::new(&targo)
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
            verification_off_verdict(&flags, &String::from_utf8_lossy(&o.stderr))
                .map(|()| trustc.clone())
        }
        Ok(o) => Err(Error::new(format!(
            "trustc at {} runs but `{} --unverified check` cannot COMPILE the probe under \
             the native-lane rustflags {flags:?} (stage2 library missing/stale — reinstall \
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
/// `RUSTUP_TOOLCHAIN=stable`), so probe STABLE's installed targets — a bare
/// `rustup target list` would resolve the repo's `trust` toolchain via
/// rust-toolchain.toml, and custom toolchains never carry rustup-managed
/// targets. When the target is absent, print the exact remediation — the release
/// is universal, and never silently ships a thinner artifact.
/// STOCK EXCEPTION (Trust lacks an x86_64-apple-darwin std: `targo --unverified
/// check --target x86_64-apple-darwin -p aterm` fails E0463).
pub fn x86_target_probe() -> Result<()> {
    let out = Command::new("rustup")
        .env("RUSTUP_TOOLCHAIN", "stable")
        .args(["target", "list", "--installed"])
        .output()
        .map_err(|e| {
            // rustup is wanted for one thing: upstream stable's x86_64-apple-darwin
            // std, which Trust lacks.
            Error::new(format!(
                "failed to run rustup ({e}); the x86_64 slice needs upstream stable's std.\n\
                 fix:  install rustup, then `rustup +stable target add x86_64-apple-darwin`"
            ))
        })?;
    if !out.status.success() {
        return Err(Error::new(format!(
            "`rustup target list --installed` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let installed = String::from_utf8_lossy(&out.stdout);
    if installed.lines().any(|l| l.trim() == "x86_64-apple-darwin") {
        return Ok(());
    }
    // FAULT only. The remedies are [`X86_SLICE_REMEDIES`], laid out by the caller: this
    // string is read on two different grids (a cut step line and a provision audit line),
    // and a hand-indented `fix:`/`or:` block gets one of them wrong by construction.
    Err(Error::new(
        "x86_64-apple-darwin target missing from the stable toolchain — a universal \
         build is impossible"
            .to_string(),
    ))
}

/// The proof, BEFORE the release's one build (`artifact::build`), that this builder can
/// both BUILD the x86_64 slice (`target`, the stable `x86_64-apple-darwin` std) and RUN
/// it (`rosetta`): the release is universal, and the build runs the slice under Rosetta
/// after it is linked (buildplan.rs, docs/DESIGN-intel-just-works-2026-09-14.md §3.1),
/// so a builder that cannot is refused before minutes of compile. The cut builds
/// nothing, so it asks neither. Both probes are injected so every arm is testable on a
/// machine that has Rosetta.
pub(crate) fn universal_gate(
    target: impl FnOnce() -> Result<()>,
    rosetta: impl FnOnce() -> std::result::Result<(), String>,
) -> Result<()> {
    // The build appends the remedies itself: the probe returns a fault, and this is the only
    // caller that must STOP, so it is the only one that has to say what to do about it.
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
/// /usr/bin/true`). `run` spawns, injected so the refusal is testable on a machine that
/// has Rosetta. The ONE Rosetta probe: the pre-build [`universal_gate`], the provision
/// audit, and the post-build slice run all ask it.
pub(crate) fn rosetta_runs(
    run: &mut dyn FnMut(&mut Command) -> std::io::Result<std::process::Output>,
) -> std::result::Result<(), String> {
    let mut rosetta = Command::new("/usr/bin/arch");
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
    let out = Command::new("df")
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

    /// A builder without Rosetta is refused before the build, naming the remedy.
    #[test]
    fn no_rosetta_is_refused_before_the_build() {
        let err = universal_gate(|| Ok(()), || Err(ROSETTA_REFUSAL.to_string()))
            .expect_err("no Rosetta must refuse a universal build");
        assert_eq!(err.to_string(), ROSETTA_REFUSAL);
        assert!(ROSETTA_REFUSAL.contains("softwareupdate --install-rosetta"));
        assert!(
            !ROSETTA_REFUSAL.contains("--arm64-only"),
            "no single-arch escape"
        );
    }

    /// A missing stable target stops before Rosetta is asked, with the target remedies
    /// laid out; both present pass.
    #[test]
    fn a_missing_target_stops_before_rosetta_is_asked() {
        let err = universal_gate(
            || Err(Error::new("x86_64-apple-darwin target missing")),
            || unreachable!("a missing target stops before Rosetta is asked"),
        )
        .expect_err("a missing target refuses");
        let err = err.to_string();
        assert!(
            err.contains("target missing") && err.contains("fix:  rustup +stable target add"),
            "{err}"
        );
        assert!(universal_gate(|| Ok(()), || Ok(())).is_ok());
    }

    /// The one Rosetta probe runs `/usr/bin/arch -x86_64 /usr/bin/true`, and a failed
    /// spawn is the same refusal as a failed run.
    #[test]
    fn the_rosetta_probe_runs_a_trivial_x86_64_program() {
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

    /// A CUT LEASES THE TOOLCHAIN IT PINNED: under the store's prefix, a
    /// lease atpkg's gc and flip gate read, naming the cutter; a toolchain outside the
    /// prefix takes none. And only a cut: nothing arms the lease unless `run_cut` does,
    /// so the tests that resolve this machine's real toolchain write nothing into its
    /// package store.
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
        assert_eq!(
            who,
            [format!(
                "aterm-release (pid {}) \u{2014} a release cut",
                std::process::id()
            )]
        );
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

    /// A git that answers by ARGV rather than by position: these tests are about
    /// which question is asked, so a positional script would pass while the gate
    /// asked something else entirely.
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
            // First MATCHING answer, consumed: the same question asked twice
            // (`rev-parse HEAD` before and after the fast-forward) gets its two
            // different truths in order.
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
        assert!(
            msg.contains("git submodule update --init --recursive"),
            "{msg}"
        );
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

    /// `checkout --detach` moves a gitlink and not the submodule checkout, and a
    /// linked worktree starts with none: the alignment updates, then CHECKS.
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
        let err = align_submodules(&git).expect_err("a stale astream must not be built");
        let msg = err.to_string();
        assert!(msg.contains("submodules could not be checked out"), "{msg}");
        assert!(
            msg.contains("could not read Username"),
            "git's own why: {msg}"
        );
        assert!(msg.contains("fix:"), "{msg}");
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

    #[test]
    fn the_commits_own_binary_is_its_cutter() {
        let note = own_cutter_with(&unrelated(), HEAD, HEAD).expect("its own");
        assert!(note.contains("itself"), "{note}");
    }

    /// Another commit's binary is refused, whatever moved between them: the release
    /// re-runs as the release commit's own cutter instead of reconciling two trees.
    #[test]
    fn another_commits_binary_is_refused_with_the_one_way_to_its_own() {
        let why = own_cutter_with(&unrelated(), OLDER, HEAD).expect_err("not its own");
        assert_eq!(
            why,
            format!("this aterm-release binary was built from {OLDER}, not {HEAD}")
        );
        let refusal = own_cutter_refusal(&why, HEAD);
        assert!(
            refusal.contains("targo --unverified ship release")
                && refusal.contains("targo clean --release -p aterm-release"),
            "{refusal}"
        );
        // a doc-only descendant is another commit too: no source closure is reasoned about
        let docs = Claimed {
            parent: OLDER,
            changed: "docs/NOTE.md\n",
        };
        assert!(own_cutter_with(&docs, OLDER, HEAD).is_err());
    }

    /// The claim commit is the claimed commit plus its ledger line and changelog roll:
    /// the claimed commit's cutter finishes it (a resume, an abandon, a recovery).
    /// NEGATIVE CONTROLS: a third path, or another parent, is another commit.
    #[test]
    fn a_claim_commit_is_finished_by_the_cutter_of_the_commit_it_claims() {
        let claim = Claimed {
            parent: OLDER,
            changed: "CHANGELOG.md\nRELEASES.ledger\n",
        };
        let note = own_cutter_with(&claim, OLDER, HEAD).expect("the claim's cutter");
        assert!(note.contains("claim commit"), "{note}");
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

    /// "Cannot tell" refuses: an unstamped or dirty-built binary is never any commit's
    /// cutter, not even through a claim commit.
    #[test]
    fn an_unstamped_or_dirty_binary_fails_closed() {
        let claim = Claimed {
            parent: "unknown",
            changed: "RELEASES.ledger\n",
        };
        let why = own_cutter_with(&claim, "unknown", HEAD).expect_err("unstamped");
        assert!(why.contains("no build commit"), "{why}");
        let why = own_cutter_with(&unrelated(), DIRTY_BUILD_COMMIT, HEAD).expect_err("dirty");
        assert!(why.contains("dirty repository source closure"), "{why}");
    }

    /// A dry-run publishes nowhere, so it proceeds — but the gate says so; a real run
    /// (or a rehearsal) is refused.
    #[test]
    fn only_a_dry_run_proceeds_with_another_commits_cutter() {
        let other = "4444444444444444444444444444444444444444";
        assert!(own_cutter_gate(&unrelated(), other, true).is_ok());
        let err = own_cutter_gate(&unrelated(), other, false).expect_err("refused");
        assert!(err.to_string().contains("fix:"), "{err}");
    }

    /// The stamp must actually be wired. A dirty test build deliberately carries
    /// the null OID; a clean one carries its real commit. Either is a bounded,
    /// fail-closed value rather than the source-export placeholder.
    #[test]
    fn this_binary_carries_a_bounded_git_stamp() {
        assert_ne!(BUILD_COMMIT, "unknown", "build.rs did not stamp the commit");
        assert_eq!(BUILD_COMMIT.len(), 40, "not a full sha: {BUILD_COMMIT}");
        assert!(
            BUILD_COMMIT.chars().all(|c| c.is_ascii_hexdigit()),
            "not hex: {BUILD_COMMIT}"
        );
    }
}

#[cfg(test)]
mod toolchain_pin_tests {
    //! THE PIN for a cut that can be split across two compilers.
    //!
    //! [`trust_stage2_bin`]'s candidate walk ends at the atpkg store's
    //! `store/trust/current`, a MUTABLE indirection that `aterm pkg install
    //! trust` repoints. It is called from at least six places spread across a
    //! half-hour cut, and before this it canonicalised afresh at every one of
    //! them — so a peer sealing a toolchain mid-cut could hand the buildplan
    //! one compiler and the publish leg another, with nothing going red. The
    //! same defect, in the same week, cost `publish/config.sh` its fix.

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

#[cfg(all(test, unix))]
mod discovery_order_tests {
    //! THE STALE RUSTUP LINK, for the cutter: `~/.rustup/toolchains/trust` -> an old
    //! stage2 while the store holds a newer build. Driven through [`discovery_order`] with the real
    //! [`atpkg::seam::stale_against_store`] against a temp layout — nothing here reads
    //! this machine's rustup, store or PATH.

    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A sysroot at `dir` whose `bin/trustc` answers `-vV` with `commit-date: <date>`,
    /// beside an executable `targo`. Every `trustc` is a HARD LINK to one script run once
    /// here, unbounded: macOS assesses a new executable on its first exec (measured ~20 s
    /// on a loaded machine — past atpkg's 5 s probe bound), and a link to an
    /// assessed file is not new.
    fn dated(root: &Path, dir: &Path, date: &str) {
        let script = root.join("dated-trustc");
        if !script.is_file() {
            fs::write(
                &script,
                "#!/bin/sh\nd=$(cat \"$(dirname \"$0\")/../commit-date\")\n\
                 echo \"rustc 1.99.0-dev (0000000 $d)\"\necho \"commit-date: $d\"\n",
            )
            .expect("write");
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");
            let _ = Command::new(&script).output();
        }
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).expect("mkdir");
        let _ = fs::remove_file(bin.join("trustc"));
        fs::hard_link(&script, bin.join("trustc")).expect("link");
        fs::write(bin.join("targo"), "#!/bin/sh\n").expect("targo");
        fs::set_permissions(bin.join("targo"), fs::Permissions::from_mode(0o755)).expect("chmod");
        fs::write(dir.join("commit-date"), date).expect("date");
    }

    #[test]
    fn a_rustup_trust_older_than_the_store_ranks_below_it() {
        let root = fs::canonicalize(env::temp_dir())
            .expect("tmp")
            .join(format!("aterm-release-order-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("mkdir");
        let layout = atpkg::store::Layout {
            prefix: root.join("prefix"),
        };
        let build = layout.build_dir("trust", 9192);
        dated(&root, &build, "2026-09-17");
        std::os::unix::fs::symlink(&build, atpkg::seam::store_current(&layout)).expect("ln");
        let stage2 = root.join("trust/build/aarch64-apple-darwin/stage2");
        dated(&root, &stage2, "2026-08-20");
        let entry = root.join("rustup/toolchains/trust");
        fs::create_dir_all(entry.parent().expect("parent")).expect("mkdir");
        std::os::unix::fs::symlink(&stage2, &entry).expect("ln rustup");
        let store_bin = layout.program_current("trust").join("bin");
        let order = || discovery_order(Some(&entry), Some(&layout), Some("/on/path".as_ref()));

        assert_eq!(
            order(),
            vec![
                store_bin.clone(),
                stage2.join("bin"),
                PathBuf::from("/on/path")
            ],
            "an older rustup entry ranks below the store, still ahead of PATH"
        );

        // Negative controls: newer, same day, undated — the entry keeps its rank.
        for (date, why) in [
            ("2026-09-18", "newer"),
            ("2026-09-17", "same day"),
            ("unknown", "undated"),
        ] {
            fs::write(stage2.join("commit-date"), date).expect("date");
            assert_eq!(
                order()[..2],
                [stage2.join("bin"), store_bin.clone()],
                "{why}"
            );
        }
        // No store at all: nothing to be older than.
        fs::write(stage2.join("commit-date"), "2026-08-20").expect("date");
        assert_eq!(
            discovery_order(Some(&entry), None, None),
            vec![stage2.join("bin")]
        );
        fs::remove_dir_all(&root).ok();
    }
}

#[cfg(test)]
mod native_lane_flag_tests {
    //! THE DRIFT THIS PINS IS INVISIBLE FROM THE MACHINE IT BREAKS.
    //!
    //! The macOS release gate reads one table name out of `.cargo/config.toml`
    //! to learn the flags its trustc probe must compile under. A renamed table
    //! is a lookup miss, and a reader that answered `[]` for it would let every
    //! probe compile under no flags and never fail on them.
    //!
    //! A comment cannot stop that happening again, so these do, on every host,
    //! with no Mac and no Trust toolchain: the config's own shape is read back
    //! and compared against the names this file knows.

    use super::*;

    /// `crates/aterm-release` sits two levels under the workspace root — the
    /// same walk `bundle.rs`'s alias test makes.
    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/aterm-release sits two levels under the root")
            .to_path_buf()
    }

    /// Every `[target.…]` table in the shipped config that carries a
    /// `rustflags` array, found WITHOUT [`TRUST_LANE_TABLES`]: the scan follows
    /// the file's shape, the way cargo's own selection ends up, rather than a
    /// key somebody typed twice.
    fn flag_carrying_tables(config: &aterm_toml::Value) -> Vec<(String, Vec<String>)> {
        config
            .get("target")
            .and_then(|targets| targets.as_table())
            .expect("[target] section")
            .iter()
            .filter_map(|(name, table)| {
                Some((
                    name.clone(),
                    table
                        .get("rustflags")?
                        .as_array()?
                        .iter()
                        .filter_map(|flag| flag.as_str().map(String::from))
                        .collect::<Vec<_>>(),
                ))
            })
            .collect()
    }

    /// THE ANTI-RENAME GATE. Rename the Trust lane's table again and the scan
    /// still finds it by its flags, the name list does not, and this fails —
    /// here, in a unit test on any host, instead of silently in a release cut
    /// on one. The table is found by `clean_islands`, the flag only the Trust
    /// lanes may see; it carries no `-Ztrust-*` flag at all (`targo trust`
    /// refuses a project-config off-switch), and this holds the file to that too.
    #[test]
    fn the_gate_knows_the_name_of_the_table_the_config_actually_carries() {
        let repo = repo_root();
        let path = repo.join(".cargo/config.toml");
        let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let config: aterm_toml::Value = text
            .parse()
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));

        let carriers = flag_carrying_tables(&config);
        let trust_lane: Vec<&(String, Vec<String>)> = carriers
            .iter()
            .filter(|(_, flags)| flags.iter().any(|flag| flag == "clean_islands"))
            .collect();
        assert!(
            !trust_lane.is_empty(),
            "no [target.…] table in {} carries `--cfg clean_islands`, the Trust lanes' \
             flag. If the table really went away, that is a deliberate edit in gates.rs \
             (native_lane_rustflags) and in this test — not a silent one. Tables with \
             rustflags: {carriers:?}",
            path.display()
        );
        for (name, _) in &trust_lane {
            assert!(
                TRUST_LANE_TABLES.contains(&name.as_str()),
                "[target.{name:?}] carries the Trust lane's flags and gates.rs does not \
                 know that name: TRUST_LANE_TABLES is {TRUST_LANE_TABLES:?}. The reader \
                 would refuse and the release's trustc probe could not run. Add the name \
                 to TRUST_LANE_TABLES."
            );
        }

        // POLICY-ONLY: no `[target.*]` table and not `[build]` may carry a
        // verification switch (`-Ztrust-verify`) in rustflags or rustdocflags.
        // `targo trust` treats project config as requirements and refuses an
        // off-switch there (any table, either key), and the `targo
        // --unverified` lane supplies its own.
        let targets = config
            .get("target")
            .and_then(|targets| targets.as_table())
            .expect("[target] section");
        let build = config.get("build");
        let tables = targets
            .iter()
            .map(|(name, table)| (format!("target.{name:?}"), table))
            .chain(build.into_iter().map(|table| ("build".to_string(), table)));
        for (name, table) in tables {
            for key in ["rustflags", "rustdocflags"] {
                let Some(flags) = table.get(key).and_then(|flags| flags.as_array()) else {
                    continue;
                };
                for flag in flags.iter().filter_map(|flag| flag.as_str()) {
                    assert!(
                        !flag.contains("trust-verify") && !flag.contains("trust_verify"),
                        "[{name}].{key} carries {flag:?}: `targo trust` refuses a \
                         project-config verification switch (\"project configuration \
                         supplies requirements, not activation …\"), and \
                         `targo --unverified` appends its own"
                    );
                }
            }
        }

        let read = native_lane_rustflags(&repo).expect("the shipped config reads");
        assert!(
            trust_lane.iter().any(|(_, flags)| *flags == read),
            "native_lane_rustflags answered {read:?}, which is no table's rustflags in \
             {}: {trust_lane:?}",
            path.display()
        );
        assert!(
            read.iter().any(|flag| flag == "clean_islands"),
            "the flags the probe would compile under lost `--cfg clean_islands`: {read:?}"
        );
    }

    /// The Trust table renamed away, the Windows cross-compile table left standing.
    #[test]
    fn a_config_whose_trust_table_vanished_fails_closed() {
        let dir = env::temp_dir().join(format!("aterm-release-lane-flags-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".cargo")).unwrap();
        fs::write(
            dir.join(".cargo/config.toml"),
            "[target.x86_64-pc-windows-gnu]\nlinker = \"x86_64-w64-mingw32-gcc\"\n",
        )
        .unwrap();

        let err = native_lane_rustflags(&dir).expect_err("a renamed table must not read as empty");
        let msg = err.to_string();
        assert!(
            msg.contains("cfg(trust_verify)"),
            "the names looked for: {msg}"
        );
        assert!(msg.contains("TRUST_LANE_TABLES"), "the fix: {msg}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A table with no `rustflags` is not a Trust-lane table, whatever its name.
    #[test]
    fn a_table_without_rustflags_is_not_a_carrier() {
        let config: aterm_toml::Value =
            "[target.'cfg(trust_verify)']\nrustdocflags = [\"--cfg\", \"clean_islands\"]\n"
                .parse()
                .unwrap();
        assert_eq!(trust_lane_rustflags(&config), None);
    }

    /// The verdict is about the COMPILER's word, not a spelling this file
    /// knows — hardcoding the off-switch here is the very drift the reader
    /// exists to avoid.
    #[test]
    fn the_verdict_reads_the_compilers_note_not_a_flag_spelling() {
        let unknown_spelling = vec!["-Zsome-future-verification-switch=off".to_string()];
        assert!(
            verification_off_verdict(
                &unknown_spelling,
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
        let canonical = vec!["--cfg".to_string(), "clean_islands".to_string()];
        let report = "=== Trust Verification Report (probe) ===\n\
                      note: Trust verification: 1 proved, 0 failed, 0 unknown, 0 timed out, \
                      0 runtime-checked out of 1 obligation(s)\n";
        let err = verification_off_verdict(&canonical, report)
            .expect_err("a compiler that verified is not an off lane");
        let msg = err.to_string();
        assert!(msg.contains("did NOT report"), "{msg}");
        assert!(msg.contains("1 proved"), "the compiler's own line: {msg}");
        assert!(msg.contains("trust-verify"), "the deciding probe: {msg}");
    }

    /// No flags is the drift itself — refused even when the note is there,
    /// because a probe that gated on nothing proves nothing about flags.
    #[test]
    fn an_empty_flag_list_is_the_drift_itself() {
        let err = verification_off_verdict(&[], "note: compiled WITHOUT Trust verification\n")
            .expect_err("no flags means the probe gated on nothing, note or no note");
        assert!(err.to_string().contains("EMPTY"), "{err}");
    }

    /// The probe source IS the gate's sensitivity. `fn main() {}` compiles
    /// identically in both lanes and trustc reports "0 obligations checked"
    /// either way, so a probe built on it can never see a flag drift.
    #[test]
    fn the_probe_source_still_carries_an_obligation() {
        assert!(
            PROBE_SRC.contains("numerator / denominator"),
            "PROBE_SRC lost its divisor-is-zero obligation; without one \
             verification_off_verdict is unfalsifiable: {PROBE_SRC}"
        );
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

    /// A cut tag's remedy is the bump its preflight twin names
    /// (the old release preflight's `fix "pub bump aterm --minor --write"`): a hand
    /// edit of `Cargo.toml` leaves `Cargo.lock` behind, and the cutter's own
    /// locked-metadata gate then refuses.
    #[test]
    fn a_tag_on_origin_names_the_pub_bump_its_preflight_twin_names() {
        let cut = Tags {
            local: true,
            on_origin: true,
        };
        let refusal = tag_free(&cut, "0.97.0")
            .expect_err("a cut version is refused")
            .to_string();
        assert!(
            refusal
                .contains("The next release is v0.98.0: `pub bump aterm --minor --write` on main"),
            "{refusal}"
        );
        assert!(
            refusal.contains(
                "on main, commit and push, then `pub stage aterm` and `pub publish aterm`"
            ),
            "`pub stage` refuses a dirty tree and exports only main's history: {refusal}"
        );
        assert!(!refusal.contains("Cargo.toml"), "{refusal}");
        let leftover = Tags {
            local: true,
            on_origin: false,
        };
        let refusal = tag_free(&leftover, "0.97.0")
            .expect_err("a leftover local tag is refused")
            .to_string();
        assert!(refusal.contains("`git tag -d v0.97.0`"), "{refusal}");
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

    fn refusal(scratch: &Scratch, origin: &Origin, version: &str) -> String {
        handoff_fixture_gate(origin, scratch.0.path(), version)
            .expect_err("the cut must be refused before the claim")
            .to_string()
    }

    /// The refusal every shape shares: which release, the README with the
    /// steps, and that nothing was spent.
    fn assert_refusal_points_at_the_steps(msg: &str, release: &str) {
        assert!(
            msg.contains(&format!("the handoff fixtures of v{release}")),
            "names the release this cut succeeds: {msg}"
        );
        assert!(msg.contains(HANDOFF_FIXTURE_README), "the steps: {msg}");
        assert!(
            msg.contains(&format!("v{release}'s own cut lands them"))
                && msg.contains(&format!("its generator at tag v{release} by hand")),
            "{msg}"
        );
        assert!(msg.contains("Nothing was claimed"), "{msg}");
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
        let msg = refusal(&scratch, &origin, "0.93.0");
        assert_refusal_points_at_the_steps(&msg, "0.92.0");
        assert!(
            msg.contains(&format!("{HANDOFF_FIXTURE_ROOT}/v0.92.0/ does not exist")),
            "names the missing directory: {msg}"
        );
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
        let msg = refusal(&scratch, &origin, "0.93.0");
        assert_refusal_points_at_the_steps(&msg, "0.92.0");
        assert!(
            msg.contains("v0.92.0/ holds no desk with a parent.toml"),
            "{msg}"
        );
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
        let msg = refusal(&scratch, &origin, "0.93.0");
        assert_refusal_points_at_the_steps(&msg, "0.92.0");
        assert!(
            msg.contains(&format!(
                "{HANDOFF_FIXTURE_GUARD} has no PINNED_DESKS row for v0.92.0"
            )),
            "names the missing row: {msg}"
        );
        assert!(
            msg.contains("desks on disk: history, twelve-panes"),
            "and what there is to pin: {msg}"
        );
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
        let msg = refusal(&scratch, &origin, "0.93.0");
        assert!(msg.contains("none of the 2 earlier versions"), "{msg}");
        assert!(msg.contains("newest 0.92.0"), "{msg}");
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
            let err = pinned_desks(broken).expect_err("an unreadable table stops the cut");
            assert!(err.to_string().contains(HANDOFF_FIXTURE_GUARD), "{err}");
        }
    }

    /// THE DESK SET. From the guard's floor on, one pinned desk is not enough:
    /// a predecessor missing a required desk — not on disk, or on disk and not
    /// pinned — refuses the cut, naming what is missing.
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
        let msg = refusal(&scratch, &origin, "0.98.0");
        assert_refusal_points_at_the_steps(&msg, "0.97.0");
        assert!(
            msg.contains("v0.97.0 lacks the required desk(s) stalled-sequence"),
            "names the missing desk: {msg}"
        );
        assert!(msg.contains("REQUIRED_DESKS"), "{msg}");
        // On disk and not pinned is missing too: the guard would not notice it
        // lost.
        scratch.desk("0.97.0", "stalled-sequence");
        let msg = refusal(&scratch, &origin, "0.98.0");
        assert!(
            msg.contains("lacks the required desk(s) stalled-sequence"),
            "an unpinned required desk: {msg}"
        );
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

    /// A DESK ADDED FOR A LATER RELEASE ASKS NOTHING OF AN EARLIER ONE. The guard
    /// gains the next release's `split-sequences` desk, required from v0.98.0 on:
    /// cutting after v0.97.0 — whose frozen fixtures can never gain it — still
    /// passes on v0.97.0's own desks, and cutting after v0.98.0 asks for it.
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
        let msg = refusal(&scratch, &origin, "0.99.0");
        assert!(
            msg.contains("lacks the required desk(s) split-sequences (required from 0.98.0 on)"),
            "{msg}"
        );
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
            // The one-floor shape this table had before: a desk with no floor.
            "const REQUIRED_DESKS: &[&str] = &[\"history\"];",
            "const REQUIRED_DESKS: &[(&str, &str)] = &[(\"0.97\", \"history\")];",
            "const REQUIRED_DESKS: &[(&str, &str)] = &[(\"history\", \"v0.97.0\")];",
        ] {
            let err = required_desks(broken).expect_err("an unreadable set stops the cut");
            assert!(err.to_string().contains(HANDOFF_FIXTURE_GUARD), "{err}");
        }
    }

    /// The guard's run, as the injected runner saw it.
    #[cfg(unix)]
    struct GuardRun {
        program: std::ffi::OsString,
        args: Vec<std::ffi::OsString>,
        dir: Option<PathBuf>,
        removed: Vec<std::ffi::OsString>,
    }

    /// Run [`handoff_fixture_guard_with`] against a runner that records the
    /// command, leaves build residue in the guard's target directory, and
    /// answers `code` with `stdout`.
    #[cfg(unix)]
    fn run_guard(scratch: &Scratch, code: i32, stdout: &str) -> (Result<usize>, GuardRun) {
        use std::os::unix::process::ExitStatusExt as _;
        let mut seen = None;
        let verdict = handoff_fixture_guard_with(
            Path::new("/stage2/bin/targo"),
            scratch.0.path(),
            &mut |command| {
                let target = scratch.0.path().join(HANDOFF_FIXTURE_GUARD_TARGET);
                fs::create_dir_all(target.join("debug/deps")).expect("residue");
                seen = Some(GuardRun {
                    program: command.get_program().to_owned(),
                    args: command.get_args().map(ToOwned::to_owned).collect(),
                    dir: command.get_current_dir().map(Path::to_path_buf),
                    removed: command
                        .get_envs()
                        .filter(|(_, value)| value.is_none())
                        .map(|(key, _)| key.to_owned())
                        .collect(),
                });
                Ok(std::process::Output {
                    status: std::process::ExitStatus::from_raw(code << 8),
                    stdout: stdout.as_bytes().to_vec(),
                    stderr: b"error[E0000]: a stand-in build error\n".to_vec(),
                })
            },
        );
        (verdict, seen.expect("the guard ran"))
    }

    /// THE GUARD RUNS IN THE CUT TREE, before the claim: a red guard refuses the
    /// cut, naming the failing test, and the guard's target directory is gone
    /// afterwards.
    #[cfg(unix)]
    #[test]
    fn a_red_fixture_guard_refuses_pre_claim() {
        let scratch = Scratch::new("guard-red");
        let stdout = "running 4 tests\n\
                      test seamless::fixture_tests::a_repainted_session_is_counted_for_the_landing_row ... ok\n\
                      test seamless::fixture_tests::every_shipped_producers_desk_adopts_exactly_and_proves ... FAILED\n\
                      \n\
                      test result: FAILED. 3 passed; 1 failed; 0 ignored; 0 measured; 6000 filtered out; finished in 0.9s\n";
        let (verdict, run) = run_guard(&scratch, 101, stdout);
        let msg = verdict.expect_err("a red guard refuses").to_string();
        assert!(
            msg.contains(
                "1 test(s) failed: seamless::fixture_tests::every_shipped_producers_desk_adopts_exactly_and_proves"
            ),
            "names the failing test: {msg}"
        );
        assert!(msg.contains("fix the CONSUMER, never a fixture"), "{msg}");
        assert!(msg.contains("Nothing was claimed"), "{msg}");
        assert_eq!(run.program, "/stage2/bin/targo");
        assert_eq!(
            run.args,
            [
                "--unverified".into(),
                "test".into(),
                "-p".into(),
                "aterm-gui".into(),
                "--lib".into(),
                "--target-dir".into(),
                scratch
                    .0
                    .path()
                    .join(HANDOFF_FIXTURE_GUARD_TARGET)
                    .into_os_string(),
                "--".into(),
                HANDOFF_FIXTURE_GUARD_FILTER.into(),
            ]
        );
        assert_eq!(
            run.dir.as_deref(),
            Some(scratch.0.path()),
            "in the cut tree"
        );
        for steering in ["RUSTFLAGS", "RUSTC", "RUSTUP_TOOLCHAIN", "CARGO_TARGET_DIR"] {
            assert!(
                run.removed.iter().any(|key| key == steering),
                "{steering} is scrubbed"
            );
        }
        assert!(
            !scratch.0.path().join(HANDOFF_FIXTURE_GUARD_TARGET).exists(),
            "the guard's target directory is removed after the run"
        );
    }

    /// A run that ran nothing checked nothing: a filter that matched no test
    /// (a renamed module) refuses rather than passing, and a build that failed
    /// before any test ran says the build's error.
    #[cfg(unix)]
    #[test]
    fn a_guard_run_that_ran_no_test_is_refused() {
        let scratch = Scratch::new("guard-empty");
        let (verdict, _) = run_guard(
            &scratch,
            0,
            "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 6282 filtered out\n",
        );
        let msg = verdict.expect_err("no test ran").to_string();
        assert!(msg.contains("no test ran"), "{msg}");
        let (verdict, _) = run_guard(&scratch, 101, "");
        let msg = verdict.expect_err("the build failed").to_string();
        assert!(msg.contains("the build or the run stopped first"), "{msg}");
        assert!(msg.contains("a stand-in build error"), "{msg}");
    }

    /// Bound to the real gate: `run_all` — which `publish.rs` runs before the
    /// claim — runs the guard in the tree it builds, with the real process
    /// runner, after every cheaper gate. Asserted as source, like the Rosetta
    /// probe: the alternative is a real cut.
    #[test]
    fn the_pre_claim_gate_runs_the_fixture_guard_last() {
        let gates = include_str!("gates.rs");
        let body = &gates[gates.find("pub fn run_all(").expect("run_all")..];
        let body = &body[..body.find("\n}\n").expect("end of run_all")];
        // Split so these assertions do not match themselves.
        let guard = body
            .find(concat!(
                "handoff_fixture_guard_gate(",
                "tree, &mut |command| command.output())"
            ))
            .expect("run_all runs the guard in the cut tree with the real runner");
        for cheaper in [
            "handoff_fixture_gate(",
            "trustc_probe(",
            "disk_gate(",
            "channel_version_gate(",
        ] {
            let at = body.find(cheaper).expect("a cheaper gate");
            assert!(at < guard, "{cheaper} runs before the guard");
        }
    }

    /// A green guard answers how many of its tests passed, for the transcript.
    #[cfg(unix)]
    #[test]
    fn a_green_fixture_guard_counts_its_tests() {
        let scratch = Scratch::new("guard-green");
        let (verdict, _) = run_guard(
            &scratch,
            0,
            "running 4 tests\n\
             test seamless::fixture_tests::every_shipped_producers_desk_adopts_exactly_and_proves ... ok\n\
             \n\
             test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 6278 filtered out; finished in 0.92s\n",
        );
        assert_eq!(verdict.expect("green"), 4);
        assert!(!scratch.0.path().join(HANDOFF_FIXTURE_GUARD_TARGET).exists());
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

    /// THE REAL TREE. For every release the real ledger records that has a
    /// fixture directory, the cut after it passes against the real fixtures and
    /// the real guard source — so the gate reads this repository's actual
    /// layout and `PINNED_DESKS`.
    ///
    /// Deliberately NOT "the live ledger tail passes": right after every cut
    /// the tail is a release whose fixtures can only be made from its fresh
    /// tag, and a test that went red for that window would fail every unrelated
    /// change in the meantime. The next cut refusing is the enforcement; this
    /// test proves the gate agrees with the tree it will judge.
    #[test]
    fn the_real_tree_passes_for_the_cut_after_each_fixtured_release() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let ledger = fs::read_to_string(repo.join(crate::ledger::LEDGER_FILE)).expect("ledger");
        let records = crate::ledger::parse(&ledger).expect("the real ledger parses");
        let mut checked = Vec::new();
        for record in &records {
            let v = record.version.as_str();
            if checked.iter().any(|c: &String| c == v)
                || !repo.join(format!("{HANDOFF_FIXTURE_ROOT}/v{v}")).is_dir()
            {
                continue;
            }
            // The ledger as it stood once `v` was its newest release.
            let last = records.iter().rposition(|r| r.version == v).expect("seen");
            let mut upto = String::new();
            for r in &records[..=last] {
                upto.push_str(&format!("{} {}\n", r.build, r.version));
            }
            let next = next_minor(v);
            assert_eq!(
                predecessor_release(&upto, &next, &all_shipped).expect("walks"),
                Some(v.to_string())
            );
            let found = handoff_fixtures_of(&GateTree::Dir(&repo), v)
                .unwrap_or_else(|e| panic!("the cut of {next} would be refused:\n{e}"));
            let mut pinned = found.pinned.clone();
            pinned.sort();
            assert_eq!(
                found.desks, pinned,
                "v{v}: every desk on disk is pinned, and every pinned desk is on disk"
            );
            checked.push(v.to_string());
        }
        // The real guard's required set, and for every desk in it a release
        // past its floor that the gate judged against it.
        let guard = fs::read_to_string(repo.join(HANDOFF_FIXTURE_GUARD)).expect("the guard");
        let required = required_desks(&guard).expect("the real guard's required set");
        for row in &required {
            assert!(
                checked
                    .iter()
                    .any(|v| release_at_least(v, &row.from).expect("orders")),
                "a release from {} on has fixtures, for {}: {checked:?}",
                row.from,
                row.desk
            );
        }
        for release in ["0.91.0", "0.92.0", "0.97.0"] {
            assert!(
                checked.iter().any(|c| c == release),
                "v{release} is in the real ledger and has fixtures: {checked:?}"
            );
        }
    }

    fn next_minor(version: &str) -> String {
        let mut parts = version.split('.');
        let major = parts.next().expect("major");
        let minor: u32 = parts.next().expect("minor").parse().expect("numeric minor");
        format!("{major}.{}.0", minor + 1)
    }
}

#[cfg(test)]
mod handoff_policy_gate_tests {
    use super::*;
    use aterm_update_core::handoff_policy::SOURCE_PATH;

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

    /// The file the next cut seals, as checked in, is one producers can follow
    /// — so a policy edit the strict read refuses fails here first, in the
    /// commit that made it. What it must keep and which builds it may name are
    /// judged against origin's tags and the ledger, by `ship check` and the cut.
    #[test]
    fn the_checked_in_policy_passes_the_gate() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/aterm-release sits two levels under the root");
        crate::bundle::handoff_policy_for_cut(root)
            .expect("the checked-in policy is one producers can follow");
    }

    /// The example the checked-in file's own comment gives — two rescues with
    /// different knobs, under their hull — is a policy the cutter accepts, and
    /// a producer that reads `[[ranges]]` follows each rescue in its range alone.
    /// Neither the file nor docs/RELEASING.md tells the release manager to take
    /// a rescue out while its builds may still be installed (r8c finding 504:
    /// both once said to remove it after one release).
    #[test]
    fn the_checked_in_policy_documents_rescues_a_cut_accepts() {
        use aterm_update_core::handoff_policy::{CarryCeiling, PolicyFile};
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/aterm-release sits two levels under the root");
        let file = fs::read_to_string(root.join(SOURCE_PATH)).expect("the checked-in policy");
        let example = file
            .split_once("# Example")
            .expect("the file gives an example")
            .1
            .lines()
            .filter_map(|line| line.strip_prefix("#   ").or((line == "#").then_some("")))
            .collect::<Vec<_>>()
            .join("\n");
        let example = format!("schema = 1\n{example}\n");
        let policy = PolicyFile::parse_for_cut(&example)
            .unwrap_or_else(|why| panic!("the example is refused: {why}\n{example}"));
        // Its ranges name builds the ledger claimed whose producers follow a
        // policy: one naming only older builds is a rescue nobody follows.
        let ledger = fs::read_to_string(root.join(crate::ledger::LEDGER_FILE))
            .expect("the checked-in ledger");
        let claimed: Vec<u64> = crate::ledger::parse(&ledger)
            .expect("the ledger parses")
            .iter()
            .map(|record| record.build)
            .collect();
        policy
            .names_producers(&claimed)
            .unwrap_or_else(|why| panic!("the example names no producer: {why}\n{example}"));
        let at = |build: u64| {
            policy
                .for_build(build)
                .map(|p| (p.carry_ceiling(), p.relaxes_land_gate()))
        };
        assert_eq!(at(1_790_585_000), Some((CarryCeiling::Repaint, false)));
        assert_eq!(at(1_790_605_000), Some((CarryCeiling::Full, true)));
        assert_eq!(at(1_790_595_000), None, "a build between the two rescues");
        let releasing =
            fs::read_to_string(root.join("docs/RELEASING.md")).expect("docs/RELEASING.md");
        for (shown, text) in [
            ("the policy file", &file),
            ("docs/RELEASING.md", &releasing),
        ] {
            for stale in [
                "take it out again in the next release",
                "be removed again in the next one",
            ] {
                assert!(!text.contains(stale), "{shown} still says {stale:?}");
            }
        }
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
    //! THE PRE-CLAIM LIVENESS GATE: a live process under a cut staging
    //! bundle refuses, "could not look" refuses, and only an answered empty look
    //! passes. The owner's aterm ran out of `dist/cut-app/aterm.app` (pid 85619) while
    //! `bundle::prepare_staging` would have `rm -rf`'d that directory unchecked.

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
        assert!(error.contains("pid 7 from"), "{error}");
        assert!(
            error.contains("/Users//a/aterm/dist/cut-1790000000.noindex/aterm.app"),
            "{error}"
        );
        assert!(error.contains("Nothing was claimed"), "{error}");
        assert!(
            !error.contains("4242"),
            "an installed copy is not a staging bundle: {error}"
        );
    }

    #[test]
    fn an_unknown_process_list_refuses() {
        let error = staged_bundle_liveness_gate(Path::new(DIST), &|| None)
            .expect_err("\"could not look\" never licenses a delete")
            .to_string();
        assert!(error.contains("cannot read the process table"), "{error}");
    }

    #[test]
    fn an_answered_look_with_nothing_under_a_staging_dir_passes() {
        let dist = Path::new(DIST);
        assert_eq!(
            staged_bundle_liveness_gate(dist, &|| Some(Vec::new())).unwrap(),
            0
        );
        // NEGATIVE CONTROL for the matcher: things that are NOT a staging bundle pass —
        // a bundle at dist/aterm.app, its rollback sibling, directories that only LOOK
        // like one, an install elsewhere, and the legacy fixed `dist/cut-app/`, which no
        // cut deletes.
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
    //! THE CUT BUILDS THE PUBLISHED COMMIT, against real git: the cut
    //! tree goes to the commit `pub publish` recorded whatever main's tip is, the
    //! operator's checkout never moves, and the transcript states how far that commit is past the newest gate
    //! receipt, by [`receipt_report`]'s predicate.

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
        /// Writes `files`, stages them with one `git add` (one per file was a git
        /// process each), and commits.
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
        fn receipts(&self) -> PathBuf {
            let dir = self.work().join(".git/aterm-verify/receipts");
            fs::create_dir_all(&dir).unwrap();
            dir
        }
        fn receipt(&self, sha: &str, verdict: &str, contract: &str) {
            fs::write(
                self.receipts().join(sha),
                format!(
                    "{RECEIPT_MAGIC}\nhead {sha}\nmode fast\nscope workspace\n\
                     verdict {verdict}\nmerge-contract {contract}\nskipped none\nwhen 1\n"
                ),
            )
            .unwrap();
        }
        /// A whole-tree PASS filed the way the gate files one:
        /// under the commit AND under its tree, one text. Returns the tree.
        fn gate_receipt(&self, sha: &str) -> String {
            let tree = self.run(&["rev-parse", &format!("{sha}^{{tree}}")]);
            let text = format!(
                "{RECEIPT_MAGIC}\nhead {sha}\ntree {tree}\nmode fast\nscope workspace\n\
                 verdict PASS\nmerge-contract yes\nskipped none\nwhen 1\n"
            );
            fs::write(self.receipts().join(sha), &text).unwrap();
            fs::write(
                self.receipts().join(format!("{RECEIPT_TREE_PREFIX}{tree}")),
                &text,
            )
            .unwrap();
            tree
        }
    }

    fn source(commit: &str) -> PublishedSource {
        PublishedSource {
            commit: commit.to_string(),
            verified_at: "2026-09-22T23:37:28+00:00".to_string(),
        }
    }

    /// THE RULING: peers pushed code after `pub publish` — the cut tree goes to the
    /// published commit anyway, their code is on main but not in the tree, and the
    /// operator's checkout does not move at all.
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
        // NEGATIVE CONTROL: the excluded code really is on main — the placement left
        // it out, rather than there being nothing to leave out.
        assert_eq!(repo.run(&["show", "origin/main:src/lib.rs"]), "v2");
        // The operator's checkout never moved: still ON main, at the tip.
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

    #[test]
    fn a_published_commit_main_does_not_carry_is_refused() {
        let repo = Repo::new("off-main");
        repo.commit("base", &[("a", "1")]);
        repo.publish_origin();
        repo.run(&["checkout", "-q", "-b", "side"]);
        let side = repo.commit("a side commit", &[("a", "2")]);
        repo.run(&["checkout", "-q", "main"]);
        let error = place_published(&repo.git(), &repo.work(), &source(&side))
            .expect_err("a commit main does not carry is no release source")
            .to_string();
        assert!(error.contains("not on origin/main"), "{error}");
        assert!(error.contains("Nothing was claimed"), "{error}");
        assert!(
            !repo.root.path().join("work-cut.noindex").exists(),
            "no cut tree was made"
        );

        let unknown = "f".repeat(40);
        let error = place_published(&repo.git(), &repo.work(), &source(&unknown))
            .expect_err("a commit this repository does not have")
            .to_string();
        assert!(error.contains("not in this repository"), "{error}");
    }

    /// The operator's uncommitted work is nothing the cut reads, so it neither
    /// blocks the cut nor is touched by it; changes inside the CUT TREE refuse,
    /// because the cutter never discards work it did not make.
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
        let error = place_published(&repo.git(), &repo.work(), &source(&second))
            .expect_err("a cut tree with changes must refuse before it moves")
            .to_string();
        assert!(error.contains("has changes"), "{error}");
        assert!(error.contains("git worktree remove --force"), "{error}");
        assert_eq!(
            fs::read_to_string(checkout.tree.join("a")).unwrap(),
            "edited in the cut tree",
            "the change was not discarded"
        );

        // THE ONE JUDGE: a file `.git/info/exclude` hides from `git status` is still a
        // change to the cut tree, as `clean_tree` would refuse it right after.
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
        assert!(
            error.contains("has changes") && error.contains("stray.txt"),
            "{error}"
        );
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
        let error = place_published(&repo.git(), &repo.work(), &source(&published))
            .expect_err("not ours")
            .to_string();
        assert!(
            error.contains("not a worktree of this repository"),
            "{error}"
        );
        assert_eq!(fs::read_to_string(foreign.join("keep")).unwrap(), "mine");
    }

    #[test]
    fn the_cut_tree_sits_beside_the_checkout_never_inside_it() {
        assert_eq!(
            cut_tree_path(Path::new("/Users//me/aterm")).unwrap(),
            PathBuf::from("/Users//me/aterm-cut.noindex")
        );
        assert!(
            !cut_tree_path(Path::new("/Users//me/aterm"))
                .unwrap()
                .starts_with("/Users//me/aterm/"),
            "inside the checkout, cargo would merge its .cargo/config.toml into the build"
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

    /// A NARROWED PASS IS NOT A GATE, as the receipt report reads it: a clean, unskipped
    /// `--changed` PASS on HEAD over a fully receipted parent leaves HEAD ungated, and
    /// the walk stops at the parent. The store is the git common dir's, which every
    /// worktree shares.
    #[test]
    fn a_change_scoped_pass_is_ungated_whatever_its_parent_carries() {
        let repo = Repo::new("receipts-changed");
        let base = repo.commit("base", &[("a", "1")]);
        repo.receipt(&base, "PASS", "yes");
        let head = repo.commit("head", &[("a", "2")]);
        fs::write(
            repo.receipts().join(&head),
            format!(
                "{RECEIPT_MAGIC}\nhead {head}\nmode fast\nscope changed\n\
                 verdict PASS\nmerge-contract no\nskipped none\nwhen 1\n"
            ),
        )
        .unwrap();
        let report = receipt_report(&repo.git(), &Stores::own(repo.receipts())).unwrap();
        assert_eq!(
            report.newest_gated,
            Some((base[..9].to_string(), "base".to_string()))
        );
        assert_eq!(report.ungated, vec![format!("{} head", &head[..9])]);
        // The control: a whole-tree receipt on HEAD is a gate.
        repo.receipt(&head, "PASS", "yes");
        let report = receipt_report(&repo.git(), &Stores::own(repo.receipts())).unwrap();
        assert_eq!(
            report.newest_gated,
            Some((head[..9].to_string(), "head".to_string()))
        );
        assert!(report.ungated.is_empty(), "{report:?}");
        let store = receipt_store(&repo.git()).unwrap();
        assert!(
            store.ends_with(".git/aterm-verify/receipts"),
            "{}",
            store.display()
        );
    }

    /// A COMMIT GATED WITH INHERITED REDS SAYS WHICH. The gate's
    /// differential verdict lets a run discharge the merge contract when every red
    /// it found is red on main with the same failure, and its receipt names each
    /// (`inherited <id>`). The report carries them for the gated commit — by its
    /// own receipt or by its tree's — and a commit gated clean carries none.
    #[test]
    fn a_commit_gated_with_inherited_reds_names_them() {
        let repo = Repo::new("receipts-inherited");
        let gated = repo.commit("gated with main's reds", &[("a", "1")]);
        let tree = repo.run(&["rev-parse", &format!("{gated}^{{tree}}")]);
        let text = format!(
            "{RECEIPT_MAGIC}\nhead {gated}\ntree {tree}\nmode fast\nscope workspace\n\
             verdict PASS\nmerge-contract yes\nskipped none\nfailures 2\n\
             fail 00000000000000aa {gated} 1 tippy lint\n\
             fail 00000000000000bb {gated} 1 -p x --lib -- flaky\nbase {gated}\n\
             inherited tippy lint\ninherited -p x --lib -- flaky\nwhen 1\n"
        );
        fs::write(repo.receipts().join(&gated), &text).unwrap();
        let report = receipt_report(&repo.git(), &Stores::own(repo.receipts())).unwrap();
        assert_eq!(report.ungated.len(), 0);
        assert_eq!(report.inherited, ["tippy lint", "-p x --lib -- flaky"]);

        // By its tree: a reworded commit over the same bytes carries them too.
        fs::remove_file(repo.receipts().join(&gated)).unwrap();
        fs::write(
            repo.receipts().join(format!("{RECEIPT_TREE_PREFIX}{tree}")),
            &text,
        )
        .unwrap();
        let report = receipt_report(&repo.git(), &Stores::own(repo.receipts())).unwrap();
        assert!(report.gated_by_tree.is_some());
        assert_eq!(report.inherited.len(), 2);

        // The control: a commit gated clean names nothing.
        let clean = repo.commit("gated clean", &[("a", "2")]);
        repo.receipt(&clean, "PASS", "yes");
        let report = receipt_report(&repo.git(), &Stores::own(repo.receipts())).unwrap();
        assert!(report.inherited.is_empty(), "{report:?}");
    }

    /// A TREE PASS WITH INHERITED REDS GATES ONLY ITS OWN BASE. It
    /// was judged against its run's merge-base with main, so it gates another
    /// commit over the same bytes only when that commit has the same parents —
    /// a reworded amend — never an identical tree on another parent, whose base
    /// may have fixed the red the tree carries. A clean pass (no `inherited`
    /// line) is about the bytes alone and gates both: the control.
    #[test]
    fn a_tree_pass_with_inherited_reds_gates_only_a_commit_on_its_base() {
        let repo = Repo::new("receipts-inherited-tree");
        let base = repo.commit("base", &[("a", "1")]);
        let gated = repo.commit("gated with main's reds", &[("a", "2")]);
        let tree = repo.run(&["rev-parse", &format!("{gated}^{{tree}}")]);
        let file = repo.receipts().join(format!("{RECEIPT_TREE_PREFIX}{tree}"));
        let text = |inherited: &str| {
            format!(
                "{RECEIPT_MAGIC}\nhead {gated}\ntree {tree}\nmode fast\nscope workspace\n\
                 verdict PASS\nmerge-contract yes\nskipped none\nfailures 1\n\
                 fail 00000000000000aa {gated} 1 tippy lint\nbase {base}\n{inherited}when 1\n"
            )
        };
        fs::write(&file, text("inherited tippy lint\n")).unwrap();

        repo.run(&["commit", "-q", "--amend", "-m", "gated, reworded"]);
        let amended = repo.run(&["rev-parse", "HEAD"]);
        let report = receipt_report(&repo.git(), &Stores::own(repo.receipts())).unwrap();
        assert!(report.ungated.is_empty(), "the same parents: {report:?}");
        assert_eq!(report.gated_by_tree.as_deref(), Some(&gated[..9]));

        repo.commit("elsewhere", &[("a", "3")]);
        let moved = repo.commit("the same bytes on another parent", &[("a", "2")]);
        assert_eq!(repo.run(&["rev-parse", &format!("{moved}^{{tree}}")]), tree);
        let report = receipt_report(&repo.git(), &Stores::own(repo.receipts())).unwrap();
        assert_eq!(report.ungated.len(), 2, "{report:?}");
        assert_eq!(
            report.newest_gated.map(|(sha, _)| sha),
            Some(amended[..9].to_string())
        );

        fs::write(&file, text("")).unwrap();
        let report = receipt_report(&repo.git(), &Stores::own(repo.receipts())).unwrap();
        assert!(report.ungated.is_empty(), "a clean pass: {report:?}");
        assert_eq!(report.gated_by_tree.as_deref(), Some(&gated[..9]));
    }

    /// The prover the gate tests' cut judges with.
    const CUT_TC: &str = "/store/trust/9192/bin trustc 43f8b339f8322f6b4cd1d9f7ae9a914eda5fef22";

    /// NOTHING SHIPS UNVERIFIED BY THE WHOLE-TREE TRUST LANE, judged
    /// from the lane's FACTS, against real git and the store the
    /// lane writes. With no facts a real cut REFUSES — naming the command that
    /// files them — and a dry run states that it would; a merge-contract PASS is
    /// not a trust run; facts that the tree's own floors judge FAIL or
    /// COULD-NOT-RUN are refused, each named, as are facts from another prover
    /// and a prover that cannot be identified; facts its floors pass verify, and
    /// say what they did not verify. A commit that changes only the floors is
    /// judged again from the same facts, its floors held to the merge contract's
    /// up-only rule against the run's: a floor lowered by hand, or re-labelled to
    /// another prover, is refused. No floor excuse passes: another
    /// prover's floor is held, and a shortfall lapsed budgets could cover is
    /// undecided, naming the re-measure.
    #[test]
    fn a_real_cut_refuses_a_tree_no_whole_tree_trust_run_passed() {
        use crate::trust_facts::{Prover, RATCHET, testing};
        const PROVER: &str = "321aaeda75478038420d97830f14724c33c8dc39";
        const DRV: &str = "targo 1.99.0-dev (321aaeda7 2026-09-17) (targo 0.1.0)";
        let repo = Repo::new("trust");
        let prover = Prover::at(&testing::prover_bin(repo.root.path(), "a", PROVER));
        let floors = |n: u32| format!("alpha\t{n}\t9\t{DRV}\n");
        let head = repo.commit("ship me", &[("src/lib.rs", "v1"), (RATCHET, &floors(5))]);
        let git = repo.git();
        let store = repo.receipts();
        let file = |summary: &str, refused: u64| {
            let mut alpha = testing::gated("alpha", summary);
            if let crate::trust_facts::Decision::Gate(m) = &mut alpha.decision {
                m.refused = refused;
                m.refused_why = "trustd unreachable: 1".into();
            }
            testing::file(&git, &store, &prover, &head, DRV, vec![alpha]);
        };
        let gate = |refuse| trust_receipt_gate_with(&git, refuse, &prover, "HEAD");
        let refused = |why: &str| {
            let err = gate(true).expect_err("a real cut refuses").to_string();
            assert!(
                err.contains("no passing whole-tree Trust advisory run"),
                "{err}"
            );
            assert!(err.contains(why), "missing {why:?}: {err}");
            assert!(
                err.contains(&format!("git worktree add --detach <dir> {head}"))
                    && err.contains("tools/trust-gate-all.sh")
                    && err.contains("Nothing was claimed."),
                "{err}"
            );
            let line = gate(false).expect("a dry run states it");
            assert!(
                line.contains("NOT verified whole-tree")
                    && line.contains("a real cut would refuse"),
                "{line}"
            );
        };
        refused("no whole-tree Trust facts for its tree");
        // A merge-contract PASS is not a whole-tree trust run.
        repo.gate_receipt(&head);
        refused("no whole-tree Trust facts");
        file("Summary: 4 proved (9 total)", 0);
        refused("say `verdict FAIL` — alpha RATCHET-REGRESSION(<5) 4/9");
        // A run trustd refused decided nothing: no pass either.
        file("Summary: 4 proved (9 total)", 1);
        refused("say `verdict COULD-NOT-RUN`");
        // The control: facts the floors pass verify.
        file("Summary: 5 proved (9 total)", 0);
        let line = gate(true).expect("verified");
        assert!(
            line.contains("VERIFIED whole-tree")
                && line.contains("5/9")
                && !line.contains("reused")
                && !line.contains("re-base"),
            "{line}"
        );
        // A commit that changes only the floors: the same facts, judged again —
        // its floors held to the merge contract's up-only rule against the
        // run's. A same-prover lowering with no reason is refused, named…
        repo.commit("floors", &[(RATCHET, &floors(3))]);
        let err = gate(true).expect_err("a floor lowered by hand").to_string();
        assert!(
            err.contains("lowers floors the merge contract's up-only rule refuses")
                && err.contains("alpha: floor 5 -> 3 under the same prover"),
            "{err}"
        );
        // …and one the rule admits verifies from the run before it.
        let floors_only = repo.commit(
            "floors",
            &[(
                RATCHET,
                &format!("alpha\t5\t9\t{DRV}\tthe floor the 0.100.0 run set\n"),
            )],
        );
        let line = gate(true).expect("verified from the run before it");
        assert!(
            line.contains(&format!(
                "reused: {} differs from it only in its floors",
                short(&floors_only)
            )),
            "{line}"
        );
        // Another prover — the same trustc commit, other bytes — has no facts.
        let other = Prover::at(&testing::prover_bin(repo.root.path(), "b", PROVER));
        let err = trust_receipt_gate_with(&git, true, &other, "HEAD")
            .expect_err("another prover's facts are not this one's")
            .to_string();
        assert!(
            err.contains("holds facts for this tree from prover"),
            "{err}"
        );
        // A prover that cannot be identified verifies nothing.
        let bare = Prover::at(&repo.root.path().join("nowhere/bin"));
        let err = trust_receipt_gate_with(&git, true, &bare, "HEAD")
            .expect_err("no identity, no match")
            .to_string();
        assert!(err.contains("cannot be identified"), "{err}");
        // A floors-only commit that re-labels the row to another prover — whose
        // floor the judge does not hold — reuses nothing: refused, named.
        let head2 = repo.commit(
            "another prover's floor",
            &[(
                RATCHET,
                "alpha\t9\t9\ttargo 1.99.0-dev (bbbbbbbbb 2026-09-17) (targo 0.1.0)\n",
            )],
        );
        let err = gate(true).expect_err("a re-labelled floor").to_string();
        assert!(
            err.contains("alpha: floor 5 -> 9 re-based onto `targo 1.99.0-dev (bbbbbbbbb"),
            "{err}"
        );
        // Floors another prover recorded ON THE MEASURED COMMIT are HELD (no
        // floor excuses): 5 proved under its 9 is refused, named — there is no
        // "re-base after the cut".
        testing::file(
            &git,
            &store,
            &prover,
            &head2,
            DRV,
            vec![testing::gated("alpha", "Summary: 5 proved (9 total)")],
        );
        let err = gate(true)
            .expect_err("another prover's floor is held")
            .to_string();
        assert!(
            err.contains("alpha RATCHET-REGRESSION(<9) 5/9") && !err.contains("re-base after"),
            "{err}"
        );
        // A run whose lapsed budgets could cover its shortfall decided nothing:
        // refused, naming the re-measure — never a pass.
        let mut lapsed = testing::gated("alpha", "Summary: 8 proved (9 total)");
        if let crate::trust_facts::Decision::Gate(m) = &mut lapsed.decision {
            m.lapsed = 2;
        }
        testing::file(&git, &store, &prover, &head2, DRV, vec![lapsed]);
        let err = gate(true)
            .expect_err("a lapsed shortfall is undecided")
            .to_string();
        assert!(
            err.contains("say `verdict COULD-NOT-RUN`")
                && err.contains("nothing was decided about: alpha")
                && err.contains("--remeasure"),
            "{err}"
        );
        assert_ne!(head2, head);
    }

    /// What `gate` answered in `check`.
    fn said(check: &Precheck, gate: &str) -> std::result::Result<String, String> {
        check
            .gates
            .iter()
            .find(|g| g.name == gate)
            .unwrap_or_else(|| panic!("no {gate} gate:\n{}", check.transcript()))
            .outcome
            .clone()
    }

    /// THE SOURCE IS NEVER PUBLISHED AHEAD OF A BINARY THAT CANNOT FOLLOW IT,
    /// against real git: `ship check` judges the COMMIT it is
    /// named — its objects and the receipts for it, never this checkout — with
    /// the cut's own gates in the cut's order, runs every gate after the
    /// cutter identity whatever the one before it said, and names each one
    /// that would refuse, in the cut's words. With the predecessor's fixtures,
    /// release notes and a handoff policy in the commit, its bundle built in this
    /// clone (the release journal names it) with a `measured yes` receipt filed under
    /// the bundle's executable digest, and TRUST facts for it from the cut's
    /// compiler, it passes, however far HEAD and the working tree have moved.
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
        // A peer's commit on top, which drops the fixtures, and a working tree
        // whose notes and policy no cut could follow: HEAD and the working tree
        // are no longer the candidate, and neither is judged for it.
        repo.run(&["rm", "-q", "-r", &fixtures]);
        let later = repo.commit("a peer's commit", &[]);
        for (path, body) in [
            (changelog::CHANGELOG_FILE, "# Changelog\n"),
            (SOURCE_PATH, "schema = 1\ncary = \"full\"\n"),
        ] {
            fs::write(repo.work().join(path), body).unwrap();
        }
        let git = repo.git();
        let prover_bin = crate::trust_facts::testing::prover_bin(
            repo.root.path(),
            "cut",
            "43f8b339f8322f6b4cd1d9f7ae9a914eda5fef22",
        );
        let cut_tc = format!(
            "{} trustc 43f8b339f8322f6b4cd1d9f7ae9a914eda5fef22",
            prover_bin.display()
        );
        let tc = || Ok(cut_tc.clone());
        // A check by the commit's own cutter: built from the commit it judges.
        let check = |rev: &str, tc: std::result::Result<String, String>| {
            let own = repo.run(&["rev-parse", rev]);
            precheck_with(&git, rev, &own, tc).expect("a releasable commit")
        };

        // Nothing measured or verified: the two receipt gates refuse — each
        // with the cut's own refusal and remedy — and the gates that read the
        // candidate's objects pass.
        let found = check(&candidate, tc());
        assert_eq!(
            (found.commit.as_str(), found.version.as_str()),
            (candidate.as_str(), "0.2.0")
        );
        let names: Vec<&str> = found.gates.iter().map(|g| g.name).collect();
        assert_eq!(
            names,
            [
                "cutter identity",
                "changelog",
                "handoff fixtures",
                "handoff policy",
                "MEASURE tier",
                "TRUST lane"
            ],
            "the cut's order, the identity first"
        );
        assert_eq!(found.refused(), ["MEASURE tier", "TRUST lane"]);
        let text = found.transcript();
        for needle in [
            &format!(
                "ok      cutter identity: built from {} itself",
                short(&candidate)
            ),
            "ok      CHANGELOG [Unreleased]: 1 entries, no '''",
            "ok      handoff fixtures of v0.1.0 checked in: 1 desks, 1 pinned",
            "ok      handoff policy a cut seals into the bundle: ",
            &format!(
                "REFUSE  MEASURE tier — no bundle of {} is built in this clone yet",
                short(&candidate)
            ),
            "builds it once and MEASURE judges those bytes; nothing is signed unmeasured",
            &format!(
                "REFUSE  TRUST lane — the tree this cut releases \
                 ({candidate}) has no passing whole-tree Trust advisory run"
            ),
            &format!(
                "git worktree add --detach <dir> {candidate}`, then `tools/trust-gate-all.sh`"
            ),
        ] {
            assert!(text.contains(needle), "missing {needle:?}:\n{text}");
        }

        // What the cut reads: the candidate's bundle, built in this clone (its
        // release journal names it), and the MEASURE receipt filed under that
        // bundle's executable digest; and trust facts for it, from its compiler.
        let tree = repo.run(&["rev-parse", &format!("{candidate}^{{tree}}")]);
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
        fs::write(
            repo.receipts()
                .join(aterm_verify::receipt::measure_key(&macho)),
            format!(
                "{RECEIPT_MAGIC}\nhead {candidate}\ntree {tree}\nmode measure\nscope workspace\n\
                 verdict PASS\nmerge-contract no\nmeasured yes\nartifact {macho}\nskipped none\n\
                 toolchain {cut_tc}\nbuild-env none\nbuild-config none\nwhen 1\n"
            ),
        )
        .unwrap();
        crate::trust_facts::testing::file(
            &git,
            &repo.receipts(),
            &crate::trust_facts::Prover::of_toolchain(&cut_tc),
            &candidate,
            "targo 1.99.0-dev (43f8b339f 2026-09-17) (targo 0.1.0)",
            Vec::new(),
        );
        let found = check(&candidate, tc());
        let text = found.transcript();
        assert!(found.refused().is_empty(), "{text}");
        assert!(
            text.contains("MEASURE tier: ")
                && text.contains("MEASURED")
                && text.contains("VERIFIED whole-tree"),
            "{text}"
        );

        // NEGATIVE CONTROLS. HEAD is judged by ITS objects and receipts: the
        // peer's commit dropped the fixtures and nothing measured it — and the
        // working tree's unfollowable notes and policy are not HEAD's.
        let head = check("HEAD", tc());
        assert_eq!(head.commit, later);
        assert_eq!(
            head.refused(),
            ["handoff fixtures", "MEASURE tier", "TRUST lane"]
        );
        assert!(
            head.transcript()
                .contains(&format!("{fixtures}/ does not exist")),
            "{}",
            head.transcript()
        );
        // A fixture directory with files but no desk directly under it: the
        // commit reader finds none, as the cut's directory walk would.
        let nodesk = repo.commit(
            "fixtures without a desk",
            &[
                (&format!("{fixtures}/notes.txt"), ""),
                (&format!("{fixtures}/history/deeper/parent.toml"), ""),
            ],
        );
        assert!(
            check(&nodesk, tc())
                .transcript()
                .contains(&format!("{fixtures}/ holds no desk with a parent.toml")),
        );
        // The release notes, as the cut judges them: an empty [Unreleased], and
        // one carrying the manifest-poisoning `'''`, refuse; a commit that
        // already carries the version's own section (a recut) is judged by it.
        for (body, refusal) in [
            (
                "## [Unreleased]\n\n## [0.1.0]\n\n- the first one\n",
                "the [Unreleased] section of CHANGELOG.md has no release notes",
            ),
            (
                "## [Unreleased]\n\n- a '''quoted''' change\n",
                "[Unreleased] contains '''",
            ),
        ] {
            let bad = repo.commit("notes", &[(changelog::CHANGELOG_FILE, body)]);
            let Err(why) = said(&check(&bad, tc()), "changelog") else {
                panic!("{body:?} passed the changelog gate");
            };
            assert!(why.contains(refusal), "{why}");
        }
        let recut = repo.commit(
            "rolled notes",
            &[(
                changelog::CHANGELOG_FILE,
                "## [Unreleased]\n\n## [0.2.0]\n\n- rolled by the first claim\n",
            )],
        );
        assert_eq!(
            said(&check(&recut, tc()), "changelog"),
            Ok("CHANGELOG [0.2.0]: 1 entries, no '''".to_string())
        );
        // The handoff policy, as strictly as the cut reads it: a misspelt key is
        // refused, naming the commit's file; no policy at all is refused with
        // what every release needs.
        let typo = repo.commit("a typo", &[(SOURCE_PATH, "schema = 1\ncary = \"full\"\n")]);
        let Err(why) = said(&check(&typo, tc()), "handoff policy") else {
            panic!("a misspelt policy key passed");
        };
        assert!(
            why.starts_with(&format!(
                "handoff-policy gate: {SOURCE_PATH} at {} is not a policy producers can follow",
                short(&typo)
            )),
            "{why}"
        );
        repo.run(&["rm", "-q", SOURCE_PATH]);
        let nopolicy = repo.commit("no policy", &[]);
        let Err(why) = said(&check(&nopolicy, tc()), "handoff policy") else {
            panic!("a commit with no policy passed");
        };
        assert!(
            why.contains("every release seals a handoff policy into its bundle"),
            "{why}"
        );
        // Another compiler than the one the trust facts name: the TRUST gate
        // refuses; no compiler at all: it refuses, saying why. MEASURE judges the
        // bytes by their digest, whatever compiles here now — the cut compiles
        // nothing.
        let other = "/store/trust/9100/bin trustc 0000000000000000000000000000000000000000";
        let found = check(&candidate, Ok(other.to_string()));
        assert_eq!(found.refused(), ["TRUST lane"], "{}", found.transcript());
        let found = check(&candidate, Err("no Trust toolchain here".to_string()));
        assert_eq!(found.refused(), ["TRUST lane"]);
        assert!(found.transcript().contains("no Trust toolchain here"));
        // The bytes, not the commit: a receipt about other bytes measures nothing.
        fs::write(
            repo.receipts()
                .join(aterm_verify::receipt::measure_key(&macho)),
            format!(
                "{RECEIPT_MAGIC}\nhead {candidate}\nmode measure\nscope workspace\n\
                 verdict PASS\nmerge-contract no\nmeasured yes\nartifact {}\nwhen 1\n",
                "ef".repeat(32)
            ),
        )
        .unwrap();
        let found = check(&candidate, tc());
        assert_eq!(found.refused(), ["MEASURE tier"], "{}", found.transcript());
        assert!(found.transcript().contains("is about other bytes"));
        // A version no release can carry, and a name that is no commit, are
        // refused before any gate.
        let patch = repo.commit(
            "a patch version",
            &[("Cargo.toml", &cargo.replace("0.2.0", "0.2.1"))],
        );
        let err = precheck_with(&git, &patch, &patch, tc()).expect_err("no release version");
        assert!(err.to_string().contains("MAJOR.MINOR.0"), "{err}");
        assert!(precheck_with(&git, "no-such-commit", &patch, tc()).is_err());
    }

    /// A rescue a release sealed stays in every later one (RELEASING.md, "The
    /// handoff policy a release seals"): against real git, a cut whose policy
    /// drops or narrows what its predecessor sealed is refused before the claim,
    /// keeping or widening it passes, retiring it under `[[retired]]` by its exact
    /// range with a reason and a date passes and says so, and a `[[retired]]`
    /// entry naming a rescue the cut keeps is refused. A rescue's range names
    /// builds that can hand over to the cut, and what the predecessor sealed is
    /// read at the commit ORIGIN's tag names — fetched when this clone lacks it —
    /// never at this clone's tag of the same name.
    #[test]
    fn a_cut_keeps_the_handoff_policy_its_predecessor_sealed() {
        use aterm_update_core::handoff_policy::SOURCE_PATH;
        let repo = Repo::new("sealed-policy");
        // The predecessor seals its one range the way every release before
        // `[[ranges]]` did; this cut writes each rescue as a `[[ranges]]` table
        // under its hull.
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
            let refused = judge(&dropped).expect_err(&dropped).to_string();
            assert!(
                refused.contains("drops what v0.1.0 sealed for builds [1791000000, 1791500000]")
                    && refused.contains(
                        "a `[[retired]]` table with `producers = [1791000000, 1791500000]`"
                    ),
                "{dropped}: {refused}"
            );
        }

        // Retired on purpose: the exact sealed range, a reason and a date, stated
        // in the line the transcript prints.
        let said = judge(&format!(
            "schema = 1\n{}",
            retired("[1791000000, 1791500000]")
        ))
        .expect("a named retirement drops the rescue");
        assert_eq!(
            said,
            "no knobs set; RETIRES the rescue v0.1.0 sealed for builds [1791000000, \
             1791500000] (2026-10-03): every build in range is past its last supported update"
        );
        for (misnamed, why) in [
            (
                format!("schema = 1\n{}", retired("[1791000000, 1791499999]")),
                "(a `[[retired]]` table names builds [1791000000, 1791499999], not that range)",
            ),
            (
                format!(
                    "{}{}",
                    ranged("[1790000000, 1792000000]"),
                    retired("[1791000000, 1791500000]")
                ),
                "but it keeps the rescue v0.1.0 sealed for them",
            ),
            (
                format!(
                    "{}{}",
                    ranged("[1791000000, 1791500000]"),
                    retired("[1791000000, 1791500000]")
                ),
                "are both rescued under `[[ranges]]` and listed under `[[retired]]`",
            ),
        ] {
            let refused = judge(&misnamed).expect_err(&misnamed).to_string();
            assert!(refused.contains(why), "{misnamed}: {refused}");
        }

        // Abandoned and recut from ANOTHER clone: origin's v0.1.0 now names a
        // commit sealing a wider rescue, which this clone never fetched, while
        // this clone's own v0.1.0 — a fetch never moves an existing tag — still
        // names the first attempt. What the release that shipped sealed is
        // origin's, so keeping only the first attempt's range drops it.
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
        let refused =
            judge(&ranged("[1791000000, 1791500000]")).expect_err("origin's tag, not this clone's");
        assert!(
            refused.to_string().contains(
                "drops what v0.1.0 sealed for builds [1791000000, 1792000000] \
                 (applies_to_producers=[1791000000, 1792000000] carry=repaint)"
            ),
            "{refused}"
        );

        // A predecessor whose tree holds no policy file sealed nothing. Its
        // ledger is RELEASES.ledger's own lines: v0.95.0, cut from a commit
        // before the policy reader, and v0.97.0, the first producer that
        // follows a policy.
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
        // With nothing to keep, a knob's range still names builds that can hand
        // over to this cut and follow a policy, by the producer's own rule: one
        // holding none the ledger claimed (a digit dropped), or only builds of
        // releases cut before the reader (v0.95.0's, which hands its sessions
        // over without looking for the file), is a rescue no producer follows,
        // and one reaching past v0.97.0's build binds the producers of every
        // release cut after it, which never had the bug — every later release
        // must keep what this one seals.
        judge(&ranged("[1790563767, 1790585188]")).expect("names v0.97.0's build");
        for (range, why) in [
            ("[179000000, 179000000]", "applies to none of the builds"),
            ("[1790563767, 1790563767]", "applies to none of the builds"),
            (
                "[1790585188, 1890585188]",
                "applies to builds after 1790585188",
            ),
        ] {
            let refused = judge(&ranged(range)).expect_err(range).to_string();
            assert!(
                refused.contains(&format!("its rescue for builds {range}: it {why}")),
                "{range}: {refused}"
            );
        }
        // A `[[retired]]` table naming nothing the predecessor sealed is the record
        // of an earlier retirement: kept, and said by no RETIRES line.
        let said = judge(&format!("schema = 1\n{}", retired("[10, 20]")))
            .expect("an earlier retirement, kept on record");
        assert_eq!(said, "no knobs set");
    }

    /// SEVERAL RESCUES, EACH KEPT UNTIL IT IS RETIRED (round eight of the update
    /// audit, r8c finding 504). A predecessor that sealed two rescues with
    /// different knobs, as `[[ranges]]` under their hull: a cut that keeps both
    /// passes, and so does one that keeps them inside wider or more conservative
    /// rescues. A cut that silently drops either one is refused before the claim,
    /// naming the dropped builds and the reason the predecessor sealed them for;
    /// one that lists it under `[[retired]]` with a reason and a date passes and
    /// says so. The single range every policy before `[[ranges]]` sealed is held
    /// the same way.
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

        let kept = judge(&sealed).unwrap_or_else(|e| panic!("both kept: {e}"));
        assert!(
            kept.contains("v0.0.8's capture panics") && kept.contains("v0.0.9's Land gate"),
            "the transcript names each rescue: {kept}"
        );
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
        let refused = judge(&only_panics)
            .expect_err("a dropped rescue")
            .to_string();
        for needle in [
            "drops what v0.1.0 sealed for builds [1791500000, 1791900000]",
            "v0.0.9's Land gate never opens",
            "[[retired]]",
        ] {
            assert!(refused.contains(needle), "missing {needle:?}: {refused}");
        }
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
        for (changed, needles) in [
            (
                narrowed,
                [
                    "drops what v0.1.0 sealed for builds [1791500000, 1791900000]",
                    "it no longer applies to build 1791800001",
                ],
            ),
            (
                weakened,
                [
                    "drops what v0.1.0 sealed for builds [1791000000, 1791100000]",
                    "it raises `carry` for build 1791000000 from \"repaint\" to \"visible\"",
                ],
            ),
        ] {
            let refused = judge(&changed).expect_err(&changed).to_string();
            for needle in needles {
                assert!(refused.contains(needle), "missing {needle:?}: {refused}");
            }
        }

        // Retired on purpose: the exact range, a reason and a date, said in the
        // line the transcript prints.
        let retired = format!(
            "{only_panics}\n[[retired]]\nproducers = [1791500000, 1791900000]\nreason = \"every \
             v0.0.9 install is past its last supported update\"\ndate = \"2026-10-03\"\n"
        );
        let said = judge(&retired).expect("a retired rescue may be dropped");
        assert!(
            said.ends_with(
                "; RETIRES the rescue v0.1.0 sealed for builds [1791500000, 1791900000] \
                 (2026-10-03): every v0.0.9 install is past its last supported update"
            ),
            "{said}"
        );

        // The one range every policy before `[[ranges]]` sealed is held the same
        // way, and kept by a `[[ranges]]` rescue that covers it.
        let legacy = Repo::new("sealed-legacy-range");
        legacy.commit(
            "v0.1.0, which seals one range",
            &[
                (crate::ledger::LEDGER_FILE, LEDGER),
                (
                    SOURCE_PATH,
                    "schema = 1\napplies_to_producers = [1791000000, 1791100000]\ncarry = \
                     \"repaint\"\n",
                ),
            ],
        );
        legacy.publish_origin();
        legacy.run(&["tag", "v0.1.0"]);
        legacy.run(&["push", "-q", "origin", "v0.1.0"]);
        let judge = |policy: &str| {
            let commit = legacy.commit(policy, &[(SOURCE_PATH, policy)]);
            handoff_policy_gate_at(&legacy.git(), &commit, "0.2.0").map(|p| p.to_string())
        };
        let refused = judge("schema = 1\n")
            .expect_err("the sealed range dropped")
            .to_string();
        assert!(
            refused.contains("drops what v0.1.0 sealed for builds [1791000000, 1791100000]"),
            "{refused}"
        );
        judge(&only_panics).unwrap_or_else(|e| panic!("kept as a [[ranges]] rescue: {e}"));
    }

    /// `ship check` answers only with the commit's OWN cutter: the check a shared checkout
    /// builds is whatever that checkout holds, and a checker whose rules are not the judged
    /// commit's could say READY for a commit whose own cutter then refuses. The cut's
    /// identity rule decides, first: a check built from the commit answers; any other
    /// REFUSES TO ANSWER — an error, before any gate, naming how to ask with the commit's
    /// own cutter.
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
        let tc = || Ok(CUT_TC.to_string());
        // The commit's own cutter answers, and says so as its first line.
        let found = precheck_with(&git, &candidate, &candidate, tc()).expect("its own");
        assert_eq!(found.gates[0].name, "cutter identity");
        assert_eq!(
            found.gates[0].outcome,
            Ok(format!(
                "cutter identity: built from {} itself",
                short(&candidate)
            ))
        );
        // NEGATIVE CONTROLS: other rules answer nothing — a checker built before the
        // judged commit (even one that moved only a doc)...
        let docs = repo.commit("a doc", &[("docs/NOTE.md", "prose\n")]);
        let refused = |stamp: &str| {
            precheck_with(&git, &docs, stamp, tc())
                .expect_err("another commit's rules answer nothing")
                .to_string()
        };
        let why = refused(&candidate);
        for needle in [
            &format!(
                "ship check will not answer for {docs}: this aterm-release binary was built \
                 from {candidate}, not {docs}"
            ),
            &format!(
                "fix:  ask with the commit's own cutter — `PUB_SOURCE_COMMIT={docs} \
                 publish/pre-promote`"
            ),
        ] {
            assert!(why.contains(needle), "missing {needle:?}:\n{why}");
        }
        // ...one that names no commit, or a dirty source closure.
        assert!(refused("unknown").contains("carries no build commit"));
        assert!(refused(DIRTY_BUILD_COMMIT).contains("dirty repository source closure"));
    }

    #[test]
    fn the_receipt_report_counts_to_the_newest_gated_commit_by_the_gated_predicate() {
        let repo = Repo::new("receipts");
        let gated = repo.commit("gated", &[("a", "1")]);
        repo.receipt(&gated, "PASS", "yes");
        let b = repo.commit("ungated one", &[("a", "2")]);
        // An older gate's receipt, and a narrowed run, do not count as gated.
        fs::write(
            repo.receipts().join(&b),
            format!(
                "aterm-verify receipt 1\nhead {b}\ntree clean\nmode fast\nscope workspace\n\
                 verdict PASS\nmerge-contract yes\nskipped none\nwhen 1\n"
            ),
        )
        .unwrap();
        let c = repo.commit("ungated two", &[("a", "3")]);
        repo.receipt(&c, "PASS", "no");
        let report = receipt_report(&repo.git(), &Stores::own(repo.receipts())).unwrap();
        assert_eq!(
            report.newest_gated,
            Some((gated[..9].to_string(), "gated".to_string()))
        );
        assert_eq!(
            report.ungated,
            vec![
                format!("{} ungated two", &c[..9]),
                format!("{} ungated one", &b[..9])
            ]
        );
        assert_eq!(report.head_verdict.as_deref(), Some("PASS"));

        // HEAD's FAIL receipt is reported, and is not a gate.
        let d = repo.commit("ungated three", &[("a", "4")]);
        repo.receipt(&d, "FAIL", "no");
        let report = receipt_report(&repo.git(), &Stores::own(repo.receipts())).unwrap();
        assert_eq!(report.ungated.len(), 3);
        assert_eq!(report.head_verdict.as_deref(), Some("FAIL"));
        assert_eq!(report.head_receipt, Some(repo.receipts().join(&d)));

        // No receipt anywhere: everything scanned is ungated, and none is claimed.
        fs::remove_dir_all(repo.receipts()).unwrap();
        let report = receipt_report(&repo.git(), &Stores::own(repo.receipts())).unwrap();
        assert_eq!(report.newest_gated, None);
        assert_eq!(report.ungated.len(), 4);
        assert_eq!(report.scanned, 4);
        assert_eq!(report.head_receipt, None);
    }

    /// THE SAME BYTES UNDER A NEW COMMIT ID ARE GATED BY THEIR TREE. A
    /// gate run files its receipt under the commit and under the commit's tree, and
    /// the report looks up both, so each way a gated tree reaches a new id counts —
    /// a MESSAGE-ONLY AMEND, an IDENTICAL-TREE REBASE onto another parent, and the
    /// report read from a SIBLING WORKTREE, which shares the store — and says whose
    /// run vouched. The negative control is the receipt filed as it was before, under
    /// the commit alone: the amended commit is then ungated.
    #[test]
    fn a_reworded_rebased_or_sibling_commit_is_gated_by_the_receipt_for_its_tree() {
        let repo = Repo::new("receipts-tree");
        let base = repo.commit("base", &[("a", "1")]);
        let gated = repo.commit("gated", &[("a", "2")]);
        let tree = repo.gate_receipt(&gated);

        repo.run(&["commit", "-q", "--amend", "-m", "gated, reworded"]);
        let amended = repo.run(&["rev-parse", "HEAD"]);
        assert_ne!(amended, gated);
        let report = receipt_report(&repo.git(), &Stores::own(repo.receipts())).unwrap();
        assert_eq!(
            report.newest_gated,
            Some((amended[..9].to_string(), "gated, reworded".to_string()))
        );
        assert!(report.ungated.is_empty(), "{report:?}");
        assert_eq!(report.gated_by_tree.as_deref(), Some(&gated[..9]));
        assert_eq!(
            report.head_verdict.as_deref(),
            Some("PASS"),
            "HEAD's tree carries a receipt"
        );

        // NEGATIVE CONTROL: filed under the commit alone, the amend is ungated.
        let tree_file = repo.receipts().join(format!("{RECEIPT_TREE_PREFIX}{tree}"));
        fs::remove_file(&tree_file).unwrap();
        let report = receipt_report(&repo.git(), &Stores::own(repo.receipts())).unwrap();
        assert_eq!(report.newest_gated, None, "{report:?}");
        assert_eq!(report.ungated.len(), 2);
        assert_eq!(report.head_verdict, None);
        repo.gate_receipt(&gated);

        // An identical-tree rebase: the same tree over a parent that is not `base`.
        let peer = repo.run(&[
            "commit-tree",
            &format!("{base}^{{tree}}"),
            "-p",
            &base,
            "-m",
            "a peer's empty change",
        ]);
        let rebased = repo.run(&["commit-tree", &tree, "-p", &peer, "-m", "gated, rebased"]);
        repo.run(&["reset", "-q", "--hard", &rebased]);
        let report = receipt_report(&repo.git(), &Stores::own(repo.receipts())).unwrap();
        assert_eq!(
            report.newest_gated,
            Some((rebased[..9].to_string(), "gated, rebased".to_string()))
        );
        assert_eq!(report.gated_by_tree.as_deref(), Some(&gated[..9]));

        // A sibling worktree at the rebased commit reads the same store and the same
        // answer.
        let linked = repo.root.path().join("linked");
        repo.run(&[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
            &rebased,
        ]);
        let sibling = Git::new(linked);
        let store = receipt_store(&sibling).unwrap();
        assert_eq!(
            store.canonicalize().unwrap(),
            repo.receipts().canonicalize().unwrap(),
            "one store for every worktree"
        );
        let report = receipt_report(&sibling, &Stores::own(store)).unwrap();
        assert_eq!(
            report.newest_gated.as_ref().map(|(sha, _)| sha.as_str()),
            Some(&rebased[..9])
        );
        assert_eq!(report.gated_by_tree.as_deref(), Some(&gated[..9]));
    }

    /// A merge's side counts when it is gated BY ITS TREE: a side reworded after its
    /// run, merged by git's own clean merge, makes the merge gated — the same
    /// clean-merge predicate, with the side's receipt found under its tree.
    #[test]
    fn a_clean_merge_of_a_side_gated_by_its_tree_counts_as_gated() {
        let repo = Repo::new("merge-tree");
        repo.commit("base", &[("a", "1"), ("b", "1")]);
        repo.run(&["checkout", "-q", "-b", "side"]);
        let side = repo.commit("gated side", &[("b", "2")]);
        repo.gate_receipt(&side);
        repo.run(&["commit", "-q", "--amend", "-m", "gated side, reworded"]);
        repo.run(&["checkout", "-q", "main"]);
        repo.commit("peer push", &[("a", "2")]);
        repo.run(&["merge", "-q", "--no-edit", "--no-ff", "side"]);
        let merge = repo.run(&["rev-parse", "HEAD"]);
        let report = receipt_report(&repo.git(), &Stores::own(repo.receipts())).unwrap();
        assert_eq!(
            report.newest_gated.as_ref().map(|(sha, _)| sha.as_str()),
            Some(&merge[..9]),
            "{report:?}"
        );
        assert!(report.ungated.is_empty());
        assert_eq!(report.gated_by_tree, None, "gated by the merge rule");
    }

    #[test]
    fn a_clean_automatic_merge_of_a_receipted_side_counts_as_gated_and_a_hand_edit_does_not() {
        let repo = Repo::new("merge");
        repo.commit("base", &[("a", "1"), ("b", "1")]);
        repo.run(&["checkout", "-q", "-b", "side"]);
        let side = repo.commit("gated side", &[("b", "2")]);
        repo.receipt(&side, "PASS", "yes");
        repo.run(&["checkout", "-q", "main"]);
        repo.commit("peer push", &[("a", "2")]);
        repo.run(&["merge", "-q", "--no-edit", "--no-ff", "side"]);
        let merge = repo.run(&["rev-parse", "HEAD"]);
        let report = receipt_report(&repo.git(), &Stores::own(repo.receipts())).unwrap();
        assert_eq!(
            report.newest_gated.as_ref().map(|(sha, _)| sha.as_str()),
            Some(&merge[..9]),
            "git's own merge of a receipted side stands for itself"
        );
        assert!(report.ungated.is_empty());

        // NEGATIVE CONTROL: the same merge with a hand edit folded in is not git's
        // own result, so it is ungated — the walk continues past it.
        fs::write(repo.work().join("a"), "edited by hand").unwrap();
        repo.run(&["add", "a"]);
        repo.run(&["commit", "-q", "--amend", "--no-edit"]);
        let report = receipt_report(&repo.git(), &Stores::own(repo.receipts())).unwrap();
        assert_eq!(report.ungated.len(), 3, "{report:?}");
        assert_eq!(report.newest_gated, None);
    }
}

#[cfg(test)]
mod trust_from_source_tests {
    use super::*;

    /// The from-source remedy builds Trust from the pinned Trust seed and seals it, and
    /// the three-way remedy carries that one spelling. Until 2026-09-30 both said
    /// `python3 x.py build --stage 2 in $HOME/trust`, which runs whatever stage0 the
    /// checkout's gitignored bootstrap.toml names — on the owner's Mac, until
    /// 2026-09-29, a "genesis" adapter running stock rustup Rust under Trust names.
    #[test]
    fn the_from_source_remedy_is_the_seed_first_recreator_then_a_seal() {
        assert!(
            TRUST_FROM_SOURCE.contains(
                "python3 scripts/recreate_bootstrap.py --stage 2 --require-seed --fresh-seed \
                 --no-register"
            ),
            "{TRUST_FROM_SOURCE}"
        );
        assert!(
            TRUST_FROM_SOURCE.contains("scripts/promote-toolchain.sh"),
            "{TRUST_FROM_SOURCE}"
        );
        assert!(
            TRUST_TOOLCHAIN_REMEDIES.contains(TRUST_FROM_SOURCE),
            "{TRUST_TOOLCHAIN_REMEDIES}"
        );
        for remedy in [TRUST_FROM_SOURCE, TRUST_TOOLCHAIN_REMEDIES] {
            assert!(!remedy.contains("x.py"), "a bare x.py build: {remedy}");
            assert!(
                !remedy.contains("--genesis"),
                "a stock-Rust stage0: {remedy}"
            );
        }
    }
}
