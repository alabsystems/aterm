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
//! lane), x86_64 rustup target probe with printed remediation (`--arm64-only`
//! opt-out), disk space, and last the cross-version handoff guard run in the
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

/// Free-disk floor for a cut. A universal release build carries two full
/// `--release` target trees (Trust arm64 + rustup x86_64, both with
/// `CARGO_PROFILE_RELEASE_DEBUG=1` for the dSYM) plus the .app/DMG staging in
/// `dist/` — 10 GiB is a conservative floor that fails BEFORE a 4-minute
/// build dies at 99% on ENOSPC.
pub const MIN_FREE_DISK_GIB: u64 = 10;

/// The Trust toolchain's stage2 tool dir (`targo`, `trustc`, `trustdoc`, …), in the
/// ONE resolution order every gate in this workspace uses (`aterm_verify::toolchain`,
/// mirrored here because this crate does not depend on that one):
/// `$TRUST_STAGE2_BIN` (an explicit development override, never fallen back from) →
/// the rustup `trust` toolchain → the atpkg store's `store/trust/current/bin` →
/// `PATH`, the rustup entry ranked BELOW the store when it is older than the store's
/// build (`discovery_order`). A candidate must carry `targo` AND `trustc`. No build
/// tree is probed: the `$HOME/trust/build/host/stage2/bin` fallback was deleted 2026-09-24
/// (a hand-made rustup link can still NAME one — the exception above; retired from the
/// delivery 2026-08-29); a from-source toolchain is reached SEALED, through the rustup
/// entry Trust's `scripts/promote-toolchain.sh` flips onto the seal.
/// Resolved to the PHYSICAL path — the protected Trust drivers refuse a symlinked
/// toolchain path — and, where the rustup entry is atpkg's VIEW (rebuilt in place on
/// every update), to the build that view presents, so a pin cannot change under a cut.
/// The gates resolve the trust-named binaries directly rather than a PATH `cargo` —
/// correctness does not depend on the operator's rustup state. (Current stage2 builds
/// ship the stock-name compatibility entries too — measured 2026-09-18 on store build
/// 9192, `bin/cargo -Vv` answers as targo and `bin/rustc` is a hard link of `trustc`.
/// Every host lane runs `targo --unverified …` FROM THIS DIR: the release cutter's
/// proof scripts, `provision`'s front door, and tools/bootstrap-publisher.sh's
/// hand-off.)
pub fn trust_stage2_bin() -> Result<PathBuf> {
    // PINNED ONCE PER PROCESS. The candidate walk below ends at the atpkg
    // store's `store/trust/current`, which is a MUTABLE indirection: `aterm pkg
    // install trust` (and `promote-toolchain.sh` behind the rustup spelling)
    // repoints it, and this function is called from at least six places spread
    // across a cut that runs for half an hour — `provision`, the buildplan, the
    // targo and trustc resolvers here, and the publish leg. Re-resolving at
    // each of them is the same defect a peer fixed in `publish/config.sh` on
    // 2026-09-10 (`rustup run trust …` re-resolved per invocation, so its
    // conformance clause could land on a different seal than the build it
    // tested): a run that resolves a moving name more than once can be split
    // across two compilers with nothing going red. Resolving once and reusing
    // the PHYSICAL path makes a concurrent seal unable to reach a running cut.
    //
    // Only SUCCESS is pinned. A failure caches nothing — there is no toolchain
    // identity to protect in that case, and a sticky "no toolchain" would
    // outlive an install the operator performs on the advice of this very
    // error.
    static PINNED: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let pinned = pin_first_success(&PINNED, resolve_trust_stage2_bin)?;
    if LEASE_ARMED.load(std::sync::atomic::Ordering::Relaxed) {
        hold_toolchain_lease(&pinned);
    }
    Ok(pinned)
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

/// THE CUT'S LEASE on the toolchain it pinned (2026-09-26, [`atpkg::lease`]), taken once,
/// at the first pin after [`arm_toolchain_lease`], and held for the life of the process —
/// the cut. The pin makes a
/// concurrent seal unable to reach a running cut; the lease makes the package manager
/// unable to take the pinned build away under it: atpkg's gc keeps a leased build, the
/// seam never re-lays a leased rustup view, and an unattended trust update waits while
/// the live toolchain is leased (four hours at the most). Between two of the cut's steps
/// no process runs from the directory, so without the lease nothing said it was in use.
///
/// A toolchain outside the atpkg prefix takes none (nothing of atpkg's reclaims it), and
/// one that cannot be taken is said and never stops the cut — which is then exactly as
/// exposed as it was before leases. Held in a static, so it is never dropped: the kernel
/// releases the lock when the cutter exits, and atpkg reaps the file of a holder that is
/// gone at its next reading.
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
/// whole property — a mid-run seal cannot change what a running cut compiles
/// with.
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

/// The two ways past a missing x86_64 slice, for a caller to append VERBATIM.
///
/// One remedy text, laid out by whichever grid is printing it. Hand-indented inside the
/// probe's own error it arrived unlabelled and mis-aligned on the audit's two-column
/// layout, directly above a `fix:` line saying the same two things again — one missing
/// rustup target printed four lines and two remedy markers.
pub const X86_SLICE_REMEDIES: &str = "rustup +stable target add x86_64-apple-darwin\n\
     or:   cut with --arm64-only — an explicit, thinner artifact";

/// The three ways to get a Trust toolchain, for a caller to append VERBATIM.
///
/// One text, appended where the caller's layout wants it, because two spellings of one
/// remedy is the drift this crate has already paid for once. The package manager
/// leads: it is the ordinary source of the pinned toolchain, and `aterm pkg doctor`
/// names the store it filled. Building from source is the developer alternative.
pub const TRUST_TOOLCHAIN_REMEDIES: &str = "aterm pkg install trust   (then `aterm pkg doctor` to confirm the store)\n\
     or, from source: python3 x.py build --stage 2 in $HOME/trust, then seal it with $HOME/trust/scripts/promote-toolchain.sh\n\
     or point TRUST_STAGE2_BIN at an existing stage2 bin dir";

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
/// WHY (measured 2026-09-24 on the owner's Mac): `~/.rustup/toolchains/trust` was a
/// hand-made link to `$HOME/trust/build/host/stage2`, a 2026-08-20 stage2, while the store
/// held build 9192 (2026-09-17). atpkg's unattended pass refused to touch a link it did
/// not lay, so this walk handed that stage2 to every cut on the machine and no toolchain
/// update reached it. (Since 2026-09-26 that pass re-points an older live build tree by
/// itself — [`atpkg::seam::live_build_tree`] — but a seal, a link put back after it and
/// one a build runs through still reach this walk.) Demoted, not dropped: a store that is
/// not there still leaves the entry ahead of PATH, which is what the walk did before.
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
/// metadata queries go through THIS binary — never a PATH `cargo`, which since
/// the stock-name purge resolves to a rustup shim with nothing behind it.
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
    /// `--arm64-only`: single-arch build; skips the x86_64 target probe.
    pub arm64_only: bool,
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
    /// This is DERIVED FROM THE CLI FLAGS, never from the environment. It
    /// replaces the former `ATERM_SKIP_CHANNEL_VERSION_GATE` env opt-out, which
    /// violated the repo rule that verification is default-on and fail-closed
    /// with no ambient skip switches: an exported variable would silently
    /// disable the gate for a REAL cut, which is precisely the failure that
    /// shipped the mismatched v0.6.0 tag.
    pub offline: bool,
    /// Only a true `--dry-run` may execute from a cutter older than the tree.
    /// A rehearsal mutates its scratch repository, so it must use the tree's
    /// own binary just like a production cut.
    pub allow_stale_cutter: bool,
    /// Will the self-check's paint smoke run? `--no-paint-smoke` means it will
    /// not, and then the scheduler tier this cut was spawned at cannot starve it
    /// ([`launchd_qos_gate`]).
    pub paint_smoke: bool,
    /// A real cut's published commit, already placed in the cut tree
    /// ([`place_published`]); `None` for a dry run or rehearsal, which build the
    /// checkout as it stands.
    pub published: Option<PublishedCheckout>,
    /// How a refusal here names running THIS run again ([`Rerun`]).
    pub rerun: Rerun,
}

/// How a refusal before the build names running THIS run again: the launcher command
/// that repeats it, and whether this process already is that launcher's job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rerun {
    /// [`crate::publish::CUT_COMMAND`] with this run's own flags: a dry run's remedy
    /// must not start a real cut, and a resume's must not start a fresh one (the
    /// journal refuses it).
    pub command: String,
    /// `ATERM_CUT_LAUNCH` is set — the launcher's plist sets it — so this cut already
    /// runs as the launcher's job, and the launcher alone changes nothing.
    pub under_launcher: bool,
}

impl Rerun {
    /// A fresh run of `kind`; `rehearse` is a `--rehearse` run's scratch channel.
    #[must_use]
    pub fn of(kind: crate::publish::CutKind, rehearse: Option<&str>) -> Self {
        use crate::publish::{CUT_COMMAND, CutKind};
        let flags = match kind {
            CutKind::Real => String::new(),
            CutKind::DryRun => " --dry-run".to_string(),
            CutKind::Rehearse => format!(" --rehearse {}", rehearse.unwrap_or("<owner/repo>")),
        };
        Self {
            command: format!("{CUT_COMMAND}{flags} --release-credentials <path>"),
            under_launcher: under_cut_launcher(),
        }
    }

    /// A `--resume` of the journaled cut.
    #[must_use]
    pub fn resume() -> Self {
        Self {
            command: format!("{} --resume", crate::publish::CUT_COMMAND),
            under_launcher: under_cut_launcher(),
        }
    }
}

/// Is this process the cut `tools/cut-launch.sh` bootstrapped? Its plist sets
/// `ATERM_CUT_LAUNCH=1` in the job's environment.
fn under_cut_launcher() -> bool {
    env::var_os("ATERM_CUT_LAUNCH").is_some_and(|value| !value.is_empty())
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
    /// false under `--arm64-only`.
    pub universal: bool,
    /// Free disk in GiB at gate time.
    pub free_disk_gib: u64,
    /// `Some(version)` when the public update channel's source tree was read and
    /// carries exactly this version; `None` when there is no channel configured,
    /// no manifest on it yet, or the gate was explicitly skipped.
    pub channel_version: Option<String>,
    /// How many running processes the staging-liveness gate read — the evidence
    /// behind "nothing runs out of a cut staging bundle".
    pub processes_checked: usize,
    /// The published commit this cut builds — `None` for a dry run or rehearsal.
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
    /// What the handoff policy this cut will seal into the bundle says
    /// ([`handoff_policy_gate`]).
    pub handoff_policy: aterm_update_core::handoff_policy::HandoffPolicy,
}

/// Run every gate, in the transcript's order, first failure wins. Cheap and
/// side-effect-free by construction (the one network touch is a fetch) with one
/// exception, a repair: [`provenance_gate`] clears `com.apple.provenance` from the
/// toolchain before it judges it. This is the always-on preflight — the optional
/// deep gate (`--gate` → tools/verify.sh --full) layers on top in chunk C, never
/// replaces this.
///
/// `git` and `tree` are the tree the cut builds — the cut tree for a real cut, the
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
    // `git pull` "dirty — commit/stash first": the right fix names the submodule.
    submodules_at_gitlinks(git)?;
    clean_tree(git)?;
    // A real cut builds the published commit: `place_published` put the cut tree
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
    cutter_identity_gate(git, &head, opts.allow_stale_cutter)?;
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
    let handoff_policy = handoff_policy_gate(tree)?;
    let gh_account = gh_auth()?;
    locked_metadata_gate(tree)?;
    let trustc = trustc_probe(tree)?;
    // The compiler runs; now prove it will not tag everything it writes.
    provenance_gate(&trustc, &opts.rerun)?;
    // ...and that the scheduler will not starve the one proof that runs after the
    // claim.
    launchd_qos_gate(opts.paint_smoke, &opts.rerun)?;
    let universal = universal_gate(opts.arm64_only, x86_target_probe, || {
        rosetta_runs(&mut |command| command.output())
    })?;
    let free_disk_gib = disk_gate(tree)?;
    let processes_checked =
        staged_bundle_liveness_gate(&state.join("dist"), &crate::bundle::running_processes)?;
    // How far the commit this cut builds is past the newest gate receipt — stated,
    // never required. Local git, so before the channel's network read. The store is
    // the git common dir's, which the cut tree shares with every other worktree.
    let receipts = receipt_report(git, &receipt_store(git)?)?;
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
        universal,
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

/// Refuse a cut whose checked-in handoff policy (`publish/handoff-policy.toml`,
/// plan P0-5 of the 2026-09-22/23 update audit) is not one the producers it is
/// meant for could follow — BEFORE the claim, so a typo costs seconds, not a
/// build number.
///
/// The bundle step copies that file into the `.app` before signing, and every
/// older aterm handing its sessions to this release reads it from there. A
/// producer IGNORES a file it cannot interpret (with one log line), which is the
/// right fallback in the field and the wrong one here: a misspelt key or value
/// would ship as a policy nobody follows, and the release it was meant to
/// rescue would behave exactly as if it had none. So the cutter reads it
/// strictly ([`crate::bundle::handoff_policy_for_cut`]): schema 1, only the keys
/// schema 1 defines, only their values, and never the reserved `seamless`.
pub fn handoff_policy_gate(
    repo: &Path,
) -> Result<aterm_update_core::handoff_policy::HandoffPolicy> {
    crate::bundle::handoff_policy_for_cut(repo)
        .map(|(_, policy)| policy)
        .map_err(|why| Error::new(format!("handoff-policy gate: {why}")))
}

/// [`handoff_policy_gate`] for a COMMIT: the policy blob the commit holds, read
/// by the same strict rule ([`crate::bundle::handoff_policy_of_text`]) — the
/// file a cut of it would seal into its bundle.
///
/// # Errors
/// The commit holds no policy, or one producers could not follow.
pub fn handoff_policy_gate_at(
    git: &dyn GitRunner,
    commit: &str,
) -> Result<aterm_update_core::handoff_policy::HandoffPolicy> {
    use aterm_update_core::handoff_policy::SOURCE_PATH;
    let refused = |why: String| Error::new(format!("handoff-policy gate: {why}"));
    let text = GateTree::Commit { git, commit }
        .text(SOURCE_PATH)
        .map_err(|e| refused(crate::bundle::handoff_policy_unreadable(&e.to_string())))?;
    crate::bundle::handoff_policy_of_text(&text, &format!("{SOURCE_PATH} at {}", short(commit)))
        .map_err(refused)
}

/// NO LIVE BUNDLE UNDER THE CUT'S `rm -rf` (2026-09-23). Refuses — pre-claim, where a
/// refusal burns no build number — when any process is executing out of a cut
/// staging directory under `dist` (a per-claim `dist/cut-<build>.noindex/`,
/// [`crate::bundle::is_staging_dir_name`]), naming the
/// pids and the path; and when the process list cannot be read at all. Returns how
/// many processes it read.
///
/// WHY. The cut deletes and rebuilds staging bundles: `bundle::assemble` removes its
/// own directory on a rebuilding resume and prunes older ones. A bundle deleted under
/// a live process re-attributes that process to a path that no longer resolves, and
/// macOS answers a code identity it cannot build by REPLACING the stored requirement
/// — resetting the grant for every copy of aterm on the Mac (three Full Disk Access
/// grants, 2026-09-21). The 2026-09-23 audit found exactly that setup armed: the
/// owner's aterm, pid 85619, running out of `dist/cut-app/aterm.app`, which the next
/// cut's `assemble` would have `rm -rf`'d with no check at all, while a comment in
/// `bundle.rs` said the staging path already prevented it.
///
/// `lister` is injected so the three answers are tests: `Some([])` passes, a live
/// process refuses, and `None` — "could not look" — refuses too, because it is the
/// one answer that can never license a delete, and pre-claim is where refusing is
/// free. `bundle::assemble` and `bundle::prune_staging_dirs` ask again at the moment
/// of deleting; this is the early refusal, not the only guard.
pub fn staged_bundle_liveness_gate(
    dist: &Path,
    lister: &dyn Fn() -> Option<Vec<crate::bundle::RunningProcess>>,
) -> Result<usize> {
    let Some(processes) = lister() else {
        return Err(Error::new(format!(
            "cannot read the process table (the kernel's executable path for each process), \
             so nothing proves that no aterm runs out of a cut staging bundle under {} — and \
             the cut deletes and rebuilds those bundles. Nothing was claimed; cut again.",
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
        "a process is running out of a cut staging bundle: {}. The cut deletes and rebuilds \
         its staging bundles, and a bundle deleted under a live process takes that \
         process's code identity with it: macOS then resets the TCC grants of every copy \
         of aterm on this Mac (2026-09-21). A staging bundle is build output, not an \
         install — quit that aterm and launch the installed one (/Applications/aterm.app), \
         then cut again. Nothing was claimed.",
        named.join("; ")
    )))
}

// ---------------------------------------------------------------------------
// the published commit (2026-09-23)
// ---------------------------------------------------------------------------

/// Where the publication engine keeps its ledger: `$PUBLICATION_ENGINE/mappings.json`,
/// default `~/publication/mappings.json` — the same location knob (a LOCATION, not a
/// skip switch) `tools/release-preflight.sh` reads. `pub publish` writes a row there
/// for every source publication: the dev commit it exported, keyed by its full sha.
/// A real cut builds the newest aterm row's commit ([`published_source`]).
/// `tools/cut-launch.sh` forwards `PUBLICATION_ENGINE` across its launchd hop, so the
/// knob means the same thing under the documented launcher as in a shell.
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
        .filter(|(sha, _)| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
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

/// The commit a real cut builds: the newest aterm row of the engine's ledger
/// ([`publication_mappings_path`], [`newest_published_source`]).
///
/// # Errors
/// An unreadable or rowless ledger is a refusal: a real cut builds the commit
/// `pub publish` recorded, and that ledger is the only record of it.
pub fn published_source() -> Result<PublishedSource> {
    let ledger = publication_mappings_path();
    let text = fs::read_to_string(&ledger).map_err(|e| {
        Error::new(format!(
            "cannot read the publication ledger {} ({e}) — a real cut builds the commit \
             `pub publish` recorded there. Pull the engine (`git -C ~/publication pull \
             --ff-only`) or point PUBLICATION_ENGINE at its checkout. Nothing was claimed.",
            ledger.display()
        ))
    })?;
    newest_published_source(&text).map_err(|e| Error::new(format!("{}: {e}", ledger.display())))
}

/// Where [`place_published`] put the commit a real cut builds.
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

/// THE CUT BUILDS THE PUBLISHED COMMIT (2026-09-23, owner ruling R2), in the cut
/// tree. Puts [`cut_tree_path`] at `source`, detached, so everything the cut reads
/// and builds — Cargo.toml's version, the changelog's notes, the sources the proof
/// snapshot takes, the cutter itself — is exactly the commit `pub publish` exported
/// and the public source release carries.
///
/// WHY NOT MAIN'S TIP. The binary and the public source are one release under one
/// tag, and until this ruling nothing bound them to one tree: the cutter
/// fast-forwarded onto `origin/main` and built whatever was there. v0.91.0 was cut
/// that way from a moving tip no gate had passed while its public source was a
/// different, verified tree. A first repair (the same day) refused a cut whenever a
/// peer's push since `pub publish` touched code — which on a main several machines
/// push to made the cut hostage to every peer. Building the published commit
/// instead makes peer pushes irrelevant to the cut: they neither block it nor leak
/// into it, and they ship in the next release.
///
/// WHY NOT THIS CHECKOUT. The first version of this function checked the published
/// commit out HERE, detached, and left it there: every refusal after the move, and
/// every finished cut, parked the checkout several agent sessions share on a days-old
/// commit, where a peer's pull, build or commit worked on stale code — and a peer
/// that "fixed" the detached HEAD mid-cut failed the build after the claim. The cut
/// tree is the cutter's own; the operator's checkout never moves, and its branch,
/// its HEAD and its uncommitted work are nothing the cut reads.
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
    let status = git_ok(&tree, &["status", "--porcelain"])?.stdout_utf8();
    if !status.trim().is_empty() {
        return Err(Error::new(format!(
            "the cut tree {} has changes — it belongs to the cutter, which never discards \
             work it did not make:\n  {}\nRemove it (`git worktree remove --force {}`), then \
             cut again; the next cut recreates it. Nothing was claimed.",
            path.display(),
            status.lines().take(5).collect::<Vec<_>>().join("\n  "),
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
    /// The reds [`Self::newest_gated`]'s receipt discharged the merge contract
    /// WITH: failures its run judged inherited from main (`inherited <id>` lines,
    /// the gate's differential verdict, 2026-09-26) — red on main with the same
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
fn receipt_store(git: &dyn GitRunner) -> Result<std::path::PathBuf> {
    let common = git_ok(
        git,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?
    .stdout_utf8();
    Ok(std::path::Path::new(common.trim()).join("aterm-verify/receipts"))
}

/// A full pass: a receipt (the gate writes one only for a clean tree) that
/// discharged the whole merge contract. An unreadable file is not a pass.
fn receipt_full_pass(receipts: &Path, sha: &str) -> bool {
    fs::read_to_string(receipts.join(sha))
        .is_ok_and(|text| receipt_field(&text, "merge-contract").as_deref() == Some("yes"))
}

/// A full pass filed under a TREE ([`passing_trees`]): the commit its run was on,
/// and whether it passed WITH reds inherited from main (`inherited` lines).
#[derive(Debug, Clone, PartialEq, Eq)]
struct TreePass {
    head: String,
    inherited: bool,
}

/// Every TREE a full pass vouches for, with the commit its run was on: the
/// `tree-<sha>` receipts whose merge contract was discharged. Read once per walk,
/// so a store with no tree receipts costs one directory read and no git call.
fn passing_trees(receipts: &Path) -> std::collections::BTreeMap<String, TreePass> {
    let Ok(entries) = fs::read_dir(receipts) else {
        return std::collections::BTreeMap::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let tree = name.strip_prefix(RECEIPT_TREE_PREFIX)?.to_string();
            let text = fs::read_to_string(entry.path()).ok()?;
            (receipt_field(&text, "merge-contract").as_deref() == Some("yes")).then(|| {
                let pass = TreePass {
                    head: receipt_field(&text, "head").unwrap_or_default(),
                    inherited: !receipt_fields(&text, "inherited").is_empty(),
                };
                (tree, pass)
            })
        })
        .collect()
}

/// Does `pass` gate `commit`, whose tree it was filed under? A clean pass is a
/// statement about the bytes, so always. One that passed WITH inherited reds
/// (2026-09-27) was a DIFFERENTIAL verdict, judged against its run's merge-base
/// with main — it excused reds main had THERE — so it gates only a commit with
/// that same base: its own, or one with the same parents (a reworded amend).
/// An identical-tree rebase onto a main that fixed an inherited red, where the
/// conflict resolution put the red back, is not gated by it.
///
/// A NEAREST BASE CHANGES NOTHING HERE (2026-09-28). A run given the opt-in
/// `--nearest-base` may have been judged against an ANCESTOR of its merge-base
/// (`crates/aterm-verify/src/nearest.rs`), and its receipt says so in a
/// `base-mode nearest <commit>` line. The cutter reads that receipt exactly as
/// any other — `merge-contract`, `inherited`, `head` — and no requirement of a
/// cut moves: its inherited reds make a tree pass gate only a commit with the
/// same parents, as here, and nothing counts it as more (or less) gated than
/// an exact-base receipt with the same lines.
///
/// So a red excused through a nearest base CARRIES THROUGH TO CUT
/// ELIGIBILITY: the evidence behind such an `inherited` line rests on the
/// nearest base's blast-radius heuristic, weaker than an exact base's. That
/// is the behaviour the opt-in was specified with; the stricter variant — this
/// function treating `inherited` from a `base-mode nearest` receipt as not
/// gating — is offered to the owner in docs/PROCESS.md §7, not implemented.
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

/// THE UNGATED RANGE, STATED (2026-09-23). Walks HEAD's first-parent history to the
/// newest commit a gate receipt vouches for — a passing receipt (merge contract
/// discharged), or a clean automatic merge of a receipted side (two parents, one
/// of them receipted, and the tree byte-equal to `git merge-tree --write-tree` of
/// the two) — and reports how many commits sit above it.
///
/// BY COMMIT, THEN BY TREE (2026-09-26). A receipt is looked up under the commit,
/// then under the commit's tree (`tree-<sha>`, filed by every gate run since that
/// date): a reworded amend, a rebase or squash that lands on the same tree, or a
/// cherry-pick that reproduces it carries bytes a run already judged under a new
/// commit id. The report names the commit whose run vouched
/// ([`ReceiptReport::gated_by_tree`]), because the receipt is about the bytes —
/// the few gates that read history (the citation gate) ran on that commit.
///
/// GATED WITH MAIN'S REDS (2026-09-26). A receipt's `merge-contract yes` means
/// nothing NEW failed: the gate judges a run against main's receipt for its base
/// and excuses a red main already had with the same failure, naming it in an
/// `inherited <id>` line. The report carries those names for the gated commit
/// ([`ReceiptReport::inherited`]) and the transcript states them, since a cut
/// from there ships them. Such a receipt gates another commit by its tree only
/// when that commit has the same base ([`tree_pass_gates`], 2026-09-27).
///
/// THIS IS THE RECEIPT CHECK, AND IT IS INLINE (2026-09-25). It reads the store
/// `tools/verify.sh` writes (`crates/aterm-verify/src/receipt.rs`) itself, in the
/// cutter's preflight, before the ledger claim. No git hook reads receipts any
/// more: the `.githooks/pre-push` that applied the same full-pass and clean-merge
/// predicate at push time was deleted under the owner's standing mandate ("I DONT
/// WANT HOOKS! NO HOOKS NO CI", 2026-07-06), and nothing here depended on it — this
/// walk never asked the hook anything, and the cut's claim pushes owed it nothing.
///
/// STATED, NOT REQUIRED. A receipt for the exact HEAD is a race the gate loses by
/// construction (it takes an hour, peers push every few minutes), so demanding one
/// would refuse every cut. What this buys is that the number is on the transcript:
/// 0.91 was cut 136 commits past the newest receipt and nothing said so. On a real
/// cut HEAD is the published commit, so the count is the published commit's. A
/// receipt on HEAD that did NOT pass is louder: the transcript warns
/// (`publish::ungated_range_lines`). What IS required on every cut is the L0
/// freeze-safety gate (`publish::run_freeze_safety_gate`), which runs itself rather
/// than trusting any record.
///
/// # Errors
/// A git failure (a history that cannot be read is not an empty one).
pub fn receipt_report(git: &dyn GitRunner, receipts: &Path) -> Result<ReceiptReport> {
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
    let trees = passing_trees(receipts);
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
            let found = [sha.to_string(), format!("{RECEIPT_TREE_PREFIX}{tree}")]
                .iter()
                .map(|key| receipts.join(key))
                .find_map(|path| fs::read_to_string(&path).ok().map(|text| (path, text)));
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
            let key = if by_commit {
                Some(sha.to_string())
            } else {
                by_tree.map(|_| format!("{RECEIPT_TREE_PREFIX}{tree}"))
            };
            let inherited = key
                .and_then(|key| fs::read_to_string(receipts.join(key)).ok())
                .map(|text| receipt_fields(&text, "inherited"))
                .unwrap_or_default();
            return Ok(ReceiptReport {
                newest_gated: Some((short(sha).to_string(), subject.to_string())),
                ungated: above,
                scanned,
                head_verdict,
                head_receipt,
                gated_by_tree: by_tree.map(|pass| short(&pass.head).to_string()),
                inherited,
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

/// The file-name prefix of a MEASURE receipt: `<store>/measure-<commit>` and
/// `<store>/measure-tree-<tree>` (`crates/aterm-verify/src/receipt.rs`
/// `MEASURE_KEY_PREFIX`, which this mirrors — no dependency edge on the gate).
const RECEIPT_MEASURE_PREFIX: &str = "measure-";

/// THE COMPILER A CUT BUILDS WITH, spelled as the gate's receipts spell the
/// one a run used (`crates/aterm-verify/src/lib.rs` `write_receipt`'s
/// `toolchain` line): `<stage2 bin dir> trustc <commit-hash>` — the pinned
/// stage2 ([`trust_stage2_bin`], canonical, as the gate's own resolution is)
/// and the `commit-hash:` its `trustc -vV` answers, `unknown` when it names
/// none (as the gate writes it).
///
/// # Errors
/// No toolchain, or a trustc that does not answer `-vV`.
pub fn cut_toolchain() -> Result<String> {
    let stage2 = trust_stage2_bin()?;
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
        .unwrap_or(UNKNOWN_COMMIT);
    Ok(format!("{} trustc {commit}", stage2.display()))
}

/// What a toolchain identity says when trustc named no commit.
const UNKNOWN_COMMIT: &str = "unknown";

/// Whether the tree a cut builds was MEASURED ([`measure_report`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeasureReport {
    /// A MEASURE receipt for HEAD or HEAD's tree says `measured yes`: the key it
    /// stands under and the commit its run was on (short) — another commit
    /// with the same bytes when it answered by the tree.
    Measured { key: String, run: String },
    /// Nothing measured it: what the store holds instead, in a few words.
    Unmeasured { why: String },
}

/// Did a gate run MEASURE the tree this cut builds, with the compiler this cut
/// builds it with? Reads the MEASURE receipts `tools/verify.sh --measure` and
/// `--full` file (2026-09-26) under `measure-<commit>`, then
/// `measure-tree-<commit^{tree}>` — `rev` is the commit judged: HEAD for the
/// cut, any commit for `ship check` ([`precheck`]) — and answers
/// [`MeasureReport::Measured`] only
/// for a receipt that is about that commit (or that tree), whole-tree, that
/// names `toolchain` — the cut's, [`cut_toolchain`] — as the compiler its run
/// used, and says `measured yes` — the gate writes that only when every stage
/// of its MEASURE tier (the release artifact's paint and spin matrices and the
/// build they judge, the typing-pacing smoke, the startup scheduler's timing
/// cases) ran green with nothing skipped. An unreadable or foreign file is no
/// measurement, never permission.
///
/// THE SAME COMPILER (2026-09-27, second review). The key is the tree, and a
/// tree is not a binary: the receipt was taken as measured whatever compiler
/// built the artifact it measured, so a tree measured under an older trustc —
/// or by a receipt naming none — passed the cut, which then shipped a binary
/// no measurement had seen. A receipt with no `toolchain` line, another one,
/// or one whose trustc named no commit (nothing to tell two builds apart by)
/// is refused, named. So is one whose run took a build environment from its
/// caller (2026-09-27, third review: its `build-env` line other than `none`,
/// or none at all) — the cut's own builds clear theirs. And so is one whose
/// builds read any cargo config file but the tree's own (2026-09-28: its
/// `build-config` line other than [`tree_build_config`], or none at all) —
/// the cut's builds read the tree's config in a fresh, empty Cargo home.
///
/// # Errors
/// A git failure naming `rev` or its tree.
pub fn measure_report(
    git: &dyn GitRunner,
    receipts: &Path,
    toolchain: &str,
    rev: &str,
) -> Result<MeasureReport> {
    let head = rev_parse(git, rev)?;
    let tree = rev_parse(git, &format!("{head}^{{tree}}"))?;
    let own_config = tree_build_config(git, &head)?;
    let mut seen = Vec::new();
    for (key, field, expect) in [
        (format!("{RECEIPT_MEASURE_PREFIX}{head}"), "head", &head),
        (
            format!("{RECEIPT_MEASURE_PREFIX}{RECEIPT_TREE_PREFIX}{tree}"),
            "tree",
            &tree,
        ),
    ] {
        let Ok(text) = fs::read_to_string(receipts.join(&key)) else {
            continue;
        };
        let shown = key.replace(expect.as_str(), short(expect));
        if receipt_field(&text, "head").is_none() {
            seen.push(format!("`{shown}` is not a receipt this cutter reads"));
            continue;
        }
        if receipt_field(&text, field).as_deref() != Some(expect.as_str()) {
            seen.push(format!("`{shown}` is about another {field}"));
            continue;
        }
        if receipt_field(&text, "scope").as_deref() != Some("workspace") {
            seen.push(format!("`{shown}` is a narrowed run's"));
            continue;
        }
        match receipt_field(&text, "toolchain") {
            None => {
                seen.push(format!(
                    "`{shown}` names no compiler, and this cut builds with `{toolchain}`"
                ));
                continue;
            }
            Some(used) if used != toolchain => {
                seen.push(format!(
                    "`{shown}` was measured with `{used}`, and this cut builds with `{toolchain}`"
                ));
                continue;
            }
            Some(_) if toolchain.ends_with(&format!(" trustc {UNKNOWN_COMMIT}")) => {
                seen.push(format!(
                    "`{shown}` and this cut's trustc name no commit (`{toolchain}`), so the \
                     compiler it measured with cannot be told from this one"
                ));
                continue;
            }
            Some(_) => {}
        }
        // THE SAME BUILD (2026-09-27, third review): the gate's release build
        // takes its caller's environment — `RUSTFLAGS`, a wrapper, a profile
        // override — and this cut's builds clear theirs (`buildplan`), so a
        // tree measured under any of it measured a binary this cut does not
        // build. Only `build-env none` measured this cut's.
        match receipt_field(&text, "build-env").as_deref() {
            Some("none") => {}
            None => {
                seen.push(format!(
                    "`{shown}` does not say what build environment its release build took, and \
                     this cut builds with none"
                ));
                continue;
            }
            Some(env) => {
                seen.push(format!(
                    "`{shown}` measured a release build under `{env}`, and this cut builds with \
                     none (its environment cleared)"
                ));
                continue;
            }
        }
        // THE SAME CONFIG FILES (2026-09-28): cargo reads `.cargo/config.toml`
        // from the directory it runs in, every ancestor and `$CARGO_HOME`, and
        // a `rustflags` or a profile there builds other code exactly as the
        // variable does. This cut's builds read the tree's own config in a
        // fresh, empty Cargo home, so only a run that read the tree's own
        // config and nothing else measured this cut's binary.
        match receipt_field(&text, "build-config") {
            Some(read) if read == own_config => {}
            None => {
                seen.push(format!(
                    "`{shown}` does not say what cargo config files its release build read, and \
                     this cut reads only the tree's own (`{own_config}`)"
                ));
                continue;
            }
            Some(read) => {
                seen.push(format!(
                    "`{shown}` measured a release build reading the cargo config files \
                     `{read}`, and this cut reads only the tree's own (`{own_config}`)"
                ));
                continue;
            }
        }
        match receipt_field(&text, "measured").as_deref() {
            Some("yes") => {
                return Ok(MeasureReport::Measured {
                    key: shown,
                    run: short(&receipt_field(&text, "head").unwrap_or_default()).to_string(),
                });
            }
            other => seen.push(format!(
                "`{shown}` says `measured {}` (verdict {})",
                other.unwrap_or("<absent>"),
                receipt_field(&text, "verdict").unwrap_or_else(|| "<absent>".to_string())
            )),
        }
    }
    Ok(MeasureReport::Unmeasured {
        why: if seen.is_empty() {
            format!(
                "no MEASURE receipt for {} or its tree {}",
                short(&head),
                short(&tree)
            )
        } else {
            seen.join("; ")
        },
    })
}

/// The `build-config` line a run of `rev`'s tree records when its builds read
/// the tree's own cargo config and no other (`aterm_verify::build_config`):
/// `repo/.cargo/<name>=<blob id>` for each of `.cargo/config` and
/// `.cargo/config.toml` the tree holds, in that order, or `none`. The blob
/// id is git's, which is why the gate records git's: the cutter reads it from
/// the tree, never from a disk a run could have edited.
///
/// # Errors
/// A git failure listing `rev`'s `.cargo`.
pub fn tree_build_config(git: &dyn GitRunner, rev: &str) -> Result<String> {
    const NAMES: [&str; 2] = ["config", "config.toml"];
    let listed = git_ok(
        git,
        &[
            "ls-tree",
            rev,
            "--",
            &format!(".cargo/{}", NAMES[0]),
            &format!(".cargo/{}", NAMES[1]),
        ],
    )?
    .stdout_utf8();
    let mut entries = Vec::new();
    for name in NAMES {
        let path = format!(".cargo/{name}");
        let blob = listed.lines().find_map(|l| {
            let (meta, p) = l.split_once('\t')?;
            let mut f = meta.split_whitespace();
            let (_mode, kind, id) = (f.next()?, f.next()?, f.next()?);
            (p == path && kind == "blob").then(|| id.to_string())
        });
        if let Some(id) = blob {
            entries.push(format!("repo/{path}={id}"));
        }
    }
    Ok(if entries.is_empty() {
        "none".to_string()
    } else {
        entries.join(" ")
    })
}

/// NOTHING SHIPS UNMEASURED (2026-09-26). The merge contract stopped running
/// the MEASURE tier that day — the fat-LTO release build, the paint and spin
/// matrices that judge it, the pacing smoke: work that measures the machine
/// and the release artifact, and that went red on load beside every push's
/// gate — so the cut is where it is required: before the ledger claim, where a
/// refusal burns no build number, the tree this cut builds must carry a
/// MEASURE receipt saying `measured yes` ([`measure_report`]). `cut --gate`
/// runs `tools/verify.sh --full` first, which files one when it measures.
///
/// `refuse` is a real cut's: without a measurement it refuses, naming what the
/// store holds and the command that measures the tree. A dry run or rehearsal
/// claims nothing, so it states the same answer as a line — including that a
/// real cut would refuse here — and goes on. Returns the transcript line.
/// Measured means with the compiler this cut builds with ([`cut_toolchain`]).
///
/// # Errors
/// A real cut of an unmeasured tree; a git failure; no compiler to name.
pub fn measure_gate(git: &dyn GitRunner, refuse: bool) -> Result<String> {
    let toolchain = cut_toolchain().map_err(|e| {
        Error::new(format!(
            "the MEASURE gate cannot name the compiler this cut builds with: {e}"
        ))
    })?;
    measure_gate_with(git, refuse, &toolchain, "HEAD")
}

/// [`measure_gate`] for the cut compiler `toolchain`, judging `rev` — HEAD for
/// the cut, the commit named for `ship check` ([`precheck`]).
fn measure_gate_with(
    git: &dyn GitRunner,
    refuse: bool,
    toolchain: &str,
    rev: &str,
) -> Result<String> {
    let head = rev_parse(git, rev)?;
    match measure_report(git, &receipt_store(git)?, toolchain, &head)? {
        MeasureReport::Measured { key, run } => Ok(format!(
            "MEASURE tier: {} MEASURED — `measured yes` in `{key}` (the run on {run}, with \
             `{toolchain}`): the release artifact's paint and spin, the pacing smoke and the \
             scheduler's timing cases ran green on these bytes, built by this compiler",
            short(&head)
        )),
        MeasureReport::Unmeasured { why } if refuse => Err(Error::new(format!(
            "the tree this cut builds ({head}) was never MEASURED: {why}. A release is not cut \
             without the gate's MEASURE tier green on the bytes it ships (the release \
             artifact's paint and spin matrices, the typing-pacing smoke, the startup \
             scheduler's timing cases — the merge contract does not run them). Measure it: \
             `git worktree add --detach <dir> {head}`, then `tools/verify.sh --measure` there \
             (any worktree of this repository shares the receipt store), or cut with --gate, \
             which runs --full; then cut again. Nothing was claimed."
        ))),
        MeasureReport::Unmeasured { why } => Ok(format!(
            "MEASURE tier: {} NOT measured ({why}) — a real cut would refuse here; \
             `tools/verify.sh --measure` on it measures it",
            short(&head)
        )),
    }
}

/// The file-name prefix of a TRUST receipt: `<store>/trust-tree-<tree>`, filed by
/// a whole-tree `tools/trust-gate-all.sh` run on a clean checkout (the script's
/// "THE TRUST RECEIPT"; no dependency edge on the script, so this mirrors it).
const RECEIPT_TRUST_PREFIX: &str = "trust-tree-";

/// The first line of every trust receipt (`tools/trust-gate-all.sh`
/// `RECEIPT_MAGIC`). A file without it is no receipt, never a permissive one.
const TRUST_RECEIPT_MAGIC: &str = "aterm-trust receipt 1";

/// `key value` from a trust receipt, `None` when absent or when the file is not
/// one.
fn trust_receipt_field(text: &str, key: &str) -> Option<String> {
    let mut lines = text.lines();
    if lines.next() != Some(TRUST_RECEIPT_MAGIC) {
        return None;
    }
    let prefix = format!("{key} ");
    lines
        .find_map(|line| line.strip_prefix(prefix.as_str()))
        .map(str::to_string)
}

/// Whether the tree a cut builds passed the whole-tree Trust advisory lane
/// ([`trust_report`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustReport {
    /// A PASS receipt for HEAD's tree from the cut's own prover: the commit its
    /// run was on (short), its `TOTAL`, what the tree's own scope list and
    /// roster keep out of the lane, and the floors another prover recorded that
    /// still wait for a re-base.
    Verified {
        run: String,
        total: String,
        not_verified: String,
        rebase_pending: String,
    },
    /// No such receipt: what the store holds instead, in a few words.
    Unverified { why: String },
}

/// Did a whole-tree run of the Trust advisory lane (`tools/trust-gate-all.sh`)
/// pass on the tree this cut builds, with the prover this cut builds with?
/// Reads `trust-tree-<rev^{tree}>` — `rev` is the commit judged: HEAD for the
/// cut, any commit for `ship check` ([`precheck`]) — and answers
/// [`TrustReport::Verified`] only
/// for a receipt about that tree, whose `trustc` line is `trustc_commit` — the
/// commit-hash of the cut's trustc ([`cut_toolchain`]) — and whose verdict is
/// `PASS`. A prover that names no commit cannot be told from another one, so
/// it verifies nothing.
///
/// # Errors
/// A git failure naming `rev`'s tree.
pub fn trust_report(
    git: &dyn GitRunner,
    receipts: &Path,
    trustc_commit: &str,
    rev: &str,
) -> Result<TrustReport> {
    let tree = rev_parse(git, &format!("{rev}^{{tree}}"))?;
    let key = format!("{RECEIPT_TRUST_PREFIX}{tree}");
    let shown = format!("{RECEIPT_TRUST_PREFIX}{}", short(&tree));
    let unverified = |why: String| Ok(TrustReport::Unverified { why });
    let Ok(text) = fs::read_to_string(receipts.join(&key)) else {
        return unverified(format!(
            "no trust receipt for its tree {} (`{shown}`)",
            short(&tree)
        ));
    };
    let field = |k: &str| trust_receipt_field(&text, k);
    if field("tree").as_deref() != Some(tree.as_str()) {
        return unverified(format!("`{shown}` is not a trust receipt about this tree"));
    }
    let proved_by = field("trustc").unwrap_or_else(|| UNKNOWN_COMMIT.to_string());
    if trustc_commit == UNKNOWN_COMMIT || proved_by == UNKNOWN_COMMIT {
        return unverified(format!(
            "`{shown}` was proved by trustc `{proved_by}` and this cut builds with trustc \
             `{trustc_commit}`: a prover that names no commit cannot be told from another"
        ));
    }
    if proved_by != trustc_commit {
        return unverified(format!(
            "`{shown}` was proved by trustc {} and this cut builds with trustc {}",
            short(&proved_by),
            short(trustc_commit)
        ));
    }
    match field("verdict").as_deref() {
        Some("PASS") => Ok(TrustReport::Verified {
            run: short(&field("head").unwrap_or_default()).to_string(),
            total: field("total").unwrap_or_else(|| "?".to_string()),
            not_verified: field("not-verified").unwrap_or_else(|| "none".to_string()),
            rebase_pending: field("rebase-pending").unwrap_or_else(|| "none".to_string()),
        }),
        other => unverified(format!(
            "`{shown}` says `verdict {}` — a library fell below its floor, had none, or \
             its verification broke, or trustd refused a library's work (COULD-NOT-RUN); \
             the run's own rows name which",
            other.unwrap_or("<absent>")
        )),
    }
}

/// NOTHING SHIPS UNVERIFIED BY THE WHOLE-TREE TRUST LANE (2026-09-27, decided
/// under the owner's standing direction of 2026-08-30 — every repo compiles under
/// Trust with verification ON — and that day's ruling: the merge contract
/// verifies only the libraries a change touched, WITH a whole-tree advisory run
/// in the release preflight). Before the ledger claim, where a refusal burns no
/// build number, the tree this cut builds must carry a PASS trust receipt from
/// the prover it builds with ([`trust_report`]) — which verifies the heavy
/// libraries the per-commit tier leaves out, so no floor drops into a release.
/// `cut --gate` runs the whole-tree lane first, which files one.
///
/// `refuse` is a real cut's; a dry run or rehearsal states the same answer as a
/// line — including that a real cut would refuse here — and goes on. Returns
/// the transcript line.
///
/// # Errors
/// A real cut of a tree with no passing receipt; a git failure; no compiler.
pub fn trust_receipt_gate(git: &dyn GitRunner, refuse: bool) -> Result<String> {
    let toolchain = cut_toolchain().map_err(|e| {
        Error::new(format!(
            "the trust receipt gate cannot name the prover this cut builds with: {e}"
        ))
    })?;
    trust_receipt_gate_with(git, refuse, trustc_commit_of(&toolchain), "HEAD")
}

/// The trustc commit-hash in a [`cut_toolchain`] identity.
fn trustc_commit_of(toolchain: &str) -> &str {
    toolchain
        .rsplit_once(" trustc ")
        .map_or(UNKNOWN_COMMIT, |(_, commit)| commit)
}

/// [`trust_receipt_gate`] for the cut prover `trustc_commit`, judging `rev` —
/// HEAD for the cut, the commit named for `ship check` ([`precheck`]).
fn trust_receipt_gate_with(
    git: &dyn GitRunner,
    refuse: bool,
    trustc_commit: &str,
    rev: &str,
) -> Result<String> {
    let head = rev_parse(git, rev)?;
    match trust_report(git, &receipt_store(git)?, trustc_commit, &head)? {
        TrustReport::Verified {
            run,
            total,
            not_verified,
            rebase_pending,
        } => {
            let mut line = format!(
                "TRUST lane: {} VERIFIED whole-tree — every library at or above its floor \
                 ({total} proved, the run on {run}, prover trustc {}); not in the lane: \
                 {not_verified}",
                short(&head),
                short(trustc_commit)
            );
            if rebase_pending != "none" {
                line.push_str(&format!(
                    "; floors another prover recorded, to re-base after the cut \
                     (`tools/trust-gate-all.sh --update-ratchet`): {rebase_pending}"
                ));
            }
            Ok(line)
        }
        TrustReport::Unverified { why } if refuse => Err(Error::new(format!(
            "the tree this cut builds ({head}) has no passing whole-tree Trust advisory run: \
             {why}. A release is not cut without every library held to its \
             tools/trust-gate-ratchet.tsv floor by the prover it is built with — the merge \
             contract verifies only the libraries a change touched, the heaviest excepted. \
             Verify it: `git worktree add --detach <dir> {head}`, then `tools/trust-gate-all.sh` \
             there (the whole tree, on a clean checkout; any worktree of this repository shares \
             the receipt store), or cut with --gate, which runs it; then cut again. Nothing was \
             claimed."
        ))),
        TrustReport::Unverified { why } => Ok(format!(
            "TRUST lane: {} NOT verified whole-tree ({why}) — a real cut would refuse here; \
             `tools/trust-gate-all.sh` on a clean checkout of it files the receipt",
            short(&head)
        )),
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
            "aterm-release · check {} (v{}): the pre-claim gates a real cut of it runs \
             that read only the commit and this repository's receipt store, asked by that \
             commit's own cutter\n",
            short(&self.commit),
            self.version
        );
        for gate in &self.gates {
            match &gate.outcome {
                Ok(line) => out.push_str(&format!("  ok      {line}\n")),
                Err(why) => out.push_str(&format!(
                    "  REFUSE  {} — the cut's own refusal: {}\n",
                    gate.name,
                    why.replace('\n', "\n          ")
                )),
            }
        }
        out
    }
}

/// THE SOURCE IS NEVER PUBLISHED AHEAD OF A BINARY THAT CANNOT FOLLOW IT
/// (2026-09-29). v0.96.0 and v0.99.0 were published as source, and only then
/// did their cuts refuse pre-claim — on the predecessor's handoff fixtures
/// (0.96), on the MEASURE and whole-tree Trust receipts (0.99) — so each went
/// out as a source release no binary could follow. `ship check` runs, for one
/// commit and without a build of aterm, the pre-claim gates of a real cut that
/// read only the commit and this repository's receipt store, in the cut's
/// order:
///
/// * the cutter identity ([`cutter_identity_gate`]'s rule) FIRST — this binary
///   is the commit's own cutter: built from it, or from a commit whose cutter
///   source closure is byte-identical. When it is not, the check REFUSES TO
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
/// * [`measure_gate`] — a MEASURE receipt for it, taken with the compiler this
///   machine's cut builds with ([`cut_toolchain`]);
/// * [`trust_receipt_gate`] — a PASS whole-tree Trust receipt for its tree,
///   from that compiler's prover.
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
            "cannot name the compiler this machine's cut builds with ({e}) — `aterm pkg \
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
    let identity = match cutter_identity_finding(
        stamp,
        &commit,
        &cutter_source_closure(git, stamp, &commit),
    ) {
        Ok(Some(note)) => note,
        Ok(None) => format!(
            "cutter identity: this check was built from {} itself",
            short(&commit)
        ),
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
                handoff_policy_gate_at(git, &commit)
                    .map(|policy| format!("handoff policy a cut seals into the bundle: {policy}")),
            ),
        },
        PrecheckGate {
            name: "MEASURE tier",
            outcome: toolchain
                .clone()
                .and_then(|tc| answer(measure_gate_with(git, true, &tc, &commit))),
        },
        PrecheckGate {
            name: "TRUST lane",
            outcome: toolchain.and_then(|tc| {
                answer(trust_receipt_gate_with(
                    git,
                    true,
                    trustc_commit_of(&tc),
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
        "a real cut of {} (v{}) would refuse before its claim, at: {}. Do not publish its \
         source yet — a source release no binary can follow is what v0.96.0 and v0.99.0 \
         became. Each REFUSE line above names its remedy; then run this again.",
        check.commit,
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
/// about the channel's contents. Cutting anyway is the behaviour that shipped the
/// mismatched v0.6.0 tag. `offline` (from `--dry-run` / `--rehearse`, never from
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
                 this cut cannot confirm the channel carries v{version}. GitHub answers 404 \
                 for both \"no such file\" and \"not authorized\", so treating this as \
                 \"nothing to compare\" would silently disable the check. Provide \
                 ~/.secrets/gh_access_token_alabsystems. There is no env opt-out: a \
                 cut that must not consult the channel is a --dry-run or a \
                 --rehearse, both of which set this gate aside explicitly."
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
pub fn clean_tree(git: &dyn GitRunner) -> Result<()> {
    let out = git_ok(git, &["status", "--porcelain"])?;
    let dirty = out.stdout_utf8();
    if dirty.trim().is_empty() {
        return Ok(());
    }
    let mut lines: Vec<&str> = dirty.lines().take(5).collect();
    if dirty.lines().count() > 5 {
        lines.push("…");
    }
    Err(Error::new(format!(
        "working tree is dirty — commit/stash first (a claim retry resets --hard):\n  {}",
        lines.join("\n  ")
    )))
}

/// Every submodule initialised and checked out at exactly the commit `HEAD`
/// records for it (2026-09-24: `vendor/astream` became a submodule, and
/// `aterm-link` path-depends on crates inside it).
///
/// `clean_tree` cannot see the worst of it: an UNINITIALISED submodule is not
/// dirty to `git status`, and cargo then stops at `failed to get astream-broker
/// as a dependency of aterm-link` minutes into the build. A submodule on
/// another commit IS dirty to `git status`, but "commit/stash first" is the
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
/// reading astream's is the same credential. (A lost claim race needs no such step:
/// `ledger::claim` resets to the published commit, whose gitlinks this already
/// placed.) Fails loudly, and leaves nothing half-done that the refusal does not name.
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

/// The repository-owned inputs that can change the `aterm-release` binary —
/// the SAME list `crates/aterm-release/build.rs` watches and judges dirty, kept
/// here so the identity gate can ask what changed between the stamp and `HEAD`.
///
/// Duplicated rather than shared because a build script cannot be imported by
/// the crate it builds; `tests/cutter_closure.rs` parses `build.rs` and refuses
/// any drift between the two lists, so the duplication is checked, not trusted.
pub const SOURCE_INPUTS: &[&str] = &[
    "crates",
    "vendor",
    ".cargo",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
];

/// What the cutter's own source closure did between the commit it was built
/// from and the commit it is about to cut.
#[derive(Debug, PartialEq, Eq)]
pub enum SourceClosure {
    /// Not one byte of [`SOURCE_INPUTS`] differs: a rebuild at `HEAD` would
    /// produce the same binary, so this one IS the tree's cutter.
    Identical,
    /// Something the binary is compiled from moved. The paths are named so the
    /// refusal can say what.
    Changed(Vec<String>),
    /// Could not be established — an absent stamp commit, a git that would not
    /// answer, a stamp that is not this line of history. "Cannot tell" is the
    /// refusing answer here for the same reason it is in
    /// [`cutter_identity_verdict`].
    Unresolvable(String),
}

/// Ask git what changed, under [`SOURCE_INPUTS`], between `stamp` and `head`.
///
/// The two must also be ONE LINE of history — either an ancestor of the other: a
/// binary built from a foreign branch that happens to share these paths is not this
/// tree's cutter, and the whole point of the stamp is provenance, not coincidence.
/// Either direction, because a real cut builds the published commit, which is
/// usually OLDER than the tip the cutter was compiled at (2026-09-23): a cutter
/// built at a descendant whose own sources did not move since is that commit's
/// cutter.
pub fn cutter_source_closure(git: &dyn GitRunner, stamp: &str, head: &str) -> SourceClosure {
    if !canonical_commit(stamp) {
        return SourceClosure::Unresolvable(format!("{stamp} is not a commit id"));
    }
    match git.git(&["cat-file", "-e", &format!("{stamp}^{{commit}}")]) {
        Ok(out) if out.success() => {}
        Ok(_) => {
            return SourceClosure::Unresolvable(format!(
                "{stamp} is not in this checkout (fetch it, or rebuild the cutter)"
            ));
        }
        Err(e) => return SourceClosure::Unresolvable(e.to_string()),
    }
    let ancestor = |a: &str, b: &str| {
        git.git(&["merge-base", "--is-ancestor", a, b])
            .map(|out| out.success())
    };
    match ancestor(stamp, head).and_then(|up| Ok(up || ancestor(head, stamp)?)) {
        Ok(true) => {}
        Ok(false) => {
            return SourceClosure::Unresolvable(format!(
                "{stamp} and {head} are not one line of history (neither is an ancestor of \
                 the other) — the cutter was built off this line of history"
            ));
        }
        Err(e) => return SourceClosure::Unresolvable(e.to_string()),
    }
    let mut args = vec!["diff", "--name-only", stamp, head, "--"];
    args.extend_from_slice(SOURCE_INPUTS);
    match git.git(&args) {
        Ok(out) if out.success() => {
            let changed: Vec<String> = out
                .stdout_utf8()
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
            if changed.is_empty() {
                SourceClosure::Identical
            } else {
                SourceClosure::Changed(changed)
            }
        }
        Ok(out) => SourceClosure::Unresolvable(out.stderr_utf8().trim().to_string()),
        Err(e) => SourceClosure::Unresolvable(e.to_string()),
    }
}

/// A real 40-hex commit id — not `"unknown"`, not git's reserved all-zero null
/// object id that `build.rs` stamps on a dirty source closure.
pub fn canonical_commit(value: &str) -> bool {
    value.len() == 40
        && value != DIRTY_BUILD_COMMIT
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The commit THIS BINARY was built from, stamped by `build.rs`; the reserved
/// null object ID means its repository source closure was dirty, and
/// `"unknown"` means the build could not establish either fact.
pub const BUILD_COMMIT: &str = env!("ATERM_RELEASE_BUILD_COMMIT");
const DIRTY_BUILD_COMMIT: &str = "0000000000000000000000000000000000000000";

/// Prove the cutter is the tree's own — the binary-side twin of the tree gates.
///
/// Every other pre-claim gate proves something about the TREE and nothing about
/// the binary doing the proving, which is the hole v0.63.0 fell through: a
/// cutter built from an older tree cut a seeded 1.07 GB image plus an
/// `-x86_64.dmg` from source that had been lean since 52c1936f, validated that
/// output against its OWN older `required_asset_names`, and passed. The tree was
/// clean the whole time.
///
/// Every remote-mutating cut must match. Only `--dry-run` may proceed with a
/// mismatch, because it stops before any upload; the mismatch is still
/// reported so its local result is not mistaken for evidence about the tree.
/// A rehearsal publishes to a scratch repository and therefore gets no
/// exemption.
pub fn cutter_identity_gate(git: &dyn GitRunner, head: &str, allow_stale: bool) -> Result<()> {
    let closure = cutter_source_closure(git, BUILD_COMMIT, head);
    match cutter_identity_verdict(BUILD_COMMIT, head, &closure, allow_stale) {
        Ok(Some(note)) => {
            println!("==> {note}");
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Pure core of [`cutter_identity_gate`] (the stamp is an input, so every case
/// is a unit test rather than a rebuild). `Ok(Some(note))` is a dry-run that
/// should say something; `Ok(None)` is silence.
pub fn cutter_identity_verdict(
    stamp: &str,
    head: &str,
    closure: &SourceClosure,
    allow_stale: bool,
) -> Result<Option<String>> {
    let what = match cutter_identity_finding(stamp, head, closure) {
        Ok(found) => return Ok(found),
        Err(what) => what,
    };
    if allow_stale {
        return Ok(Some(format!(
            "cutter identity: {what} — allowed because this dry-run publishes nowhere, \
             but it is running OTHER code than the tree"
        )));
    }
    Err(Error::new(format!(
        "{what}.\n\
         fix:  re-run the cut — cargo rebuilds the cutter from this tree automatically.\n\
         \x20     To force it: `targo clean --release -p aterm-release`. THE PROFILE IS \
         NOT OPTIONAL — the `ship` alias is `run --release`, so the cutter is a release \
         artifact and `clean -p` without `--release` cleans the dev profile and removes \
         nothing (measured on m3, 2026-09-17: 0 files vs 166). `clean` takes no lane \
         flag; `targo --unverified clean` is refused.\n\
         why:  v0.63.0 was cut by a binary older than its own source and shipped the \
         seeded image the tree had already retired"
    )))
}

/// The cutter-identity RULE, shared by the cut ([`cutter_identity_verdict`])
/// and `ship check` ([`precheck`]): `Ok(None)` — the binary was built from
/// `head` itself; `Ok(Some(note))` — built from another commit, with not one
/// byte of the cutter's own source closure moved between them, so it is
/// `head`'s cutter all the same; `Err(what)` — its rules are not `head`'s, and
/// why. Each caller adds its own remedy.
fn cutter_identity_finding(
    stamp: &str,
    head: &str,
    closure: &SourceClosure,
) -> std::result::Result<Option<String>, String> {
    if stamp == head {
        return Ok(None);
    }
    // THE STAMP IS A PROXY; THE CLOSURE IS THE PREDICATE. What this gate owes is
    // "the rules this binary enforces are the rules this tree declares", and sha
    // equality was only ever a conservative way to say it. On a repository several
    // machines push to, that proxy costs releases: a peer's push to anything at all
    // — a doc, the changelog, another program's crate — moves HEAD, and the cutter
    // that was compiled from the previous tip is refused although not one byte it
    // was built from has moved. Ask the real question instead: if nothing under
    // SOURCE_INPUTS differs between the stamp and HEAD, a rebuild here would emit
    // this same binary, and v0.63.0's hole (a cutter validating its output against
    // its own older rules) stays shut because its rules ARE this tree's.
    if let SourceClosure::Identical = closure
        && canonical_commit(stamp)
    {
        return Ok(Some(format!(
            "cutter identity: built from {stamp}, tree is at {head} — nothing under the \
             cutter's own source closure moved between them, so this binary is what this \
             tree builds"
        )));
    }
    // An unknown stamp is NOT a pass. "Cannot tell" is how the stale cutter got
    // through, so it is the refusing answer here, exactly as an unreachable
    // channel is for `channel_version_gate`.
    Err(if stamp == "unknown" {
        "this aterm-release binary carries no build commit (built without git, or from an export)"
            .to_string()
    } else if stamp == DIRTY_BUILD_COMMIT {
        "this aterm-release binary was built from a dirty repository source closure".to_string()
    } else {
        let detail = match closure {
            SourceClosure::Changed(paths) => {
                let shown: Vec<&str> = paths.iter().take(3).map(String::as_str).collect();
                format!(
                    " — {} of the cutter's own source files moved between them ({}{})",
                    paths.len(),
                    shown.join(", "),
                    if paths.len() > shown.len() {
                        ", …"
                    } else {
                        ""
                    }
                )
            }
            SourceClosure::Unresolvable(why) => {
                format!(" — and what moved between them cannot be established: {why}")
            }
            SourceClosure::Identical => String::new(),
        };
        format!(
            "this aterm-release binary was built from {stamp}, but the tree is at \
             {head}{detail}"
        )
    })
}

/// Apply the real-mutation cutter check to the checkout the caller is about
/// to act from. Resume, recovery, abandon and yank do not enter
/// [`run_all`], so each of those paths uses this shared boundary before its
/// first remote mutation.
pub fn current_cutter_identity_gate(git: &dyn GitRunner) -> Result<()> {
    cutter_identity_gate(git, &rev_parse(git, "HEAD")?, false)
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
/// pinned (plan P0-7 of the 2026-09-22/23 update audit).
///
/// An installed release N updates to N+1 by handing its sessions to the new
/// build, so that hop is decided by N's frozen producer and N+1's consumer. The
/// only thing that checks N+1's consumer against what N really writes is N's
/// fixture directory, run by the guard in [`HANDOFF_FIXTURE_GUARD`]. Until
/// 2026-09-24 adding it was a checklist step that nothing checked, and a skipped
/// one is invisible: without the directory every handoff test for that hop
/// writes and reads with the same build, the shape that stayed green while the
/// 2026-09-22/23 hop failed in the field. Refusing here, before the claim, makes
/// a missing directory cost seconds rather than a release whose consumer nobody
/// checked against what its predecessor sends.
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
enum GateTree<'a> {
    Dir(&'a Path),
    Commit {
        git: &'a dyn GitRunner,
        commit: &'a str,
    },
}

impl GateTree<'_> {
    /// The text of the tracked file `path`.
    fn text(&self, path: &str) -> Result<String> {
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
    fn desks(&self, dir: &str) -> Result<Option<Vec<String>>> {
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
    let ledger_text = tree.text(crate::ledger::LEDGER_FILE)?;
    // ONE listing, not a probe per version: walking back past an abandoned
    // claim must not cost a network round trip per step.
    let listing = git_ok(git, &["ls-remote", "--tags", "origin"]).map_err(|e| {
        Error::new(format!(
            "handoff-fixture gate: cannot list origin's tags, which is how it tells the \
             releases that shipped from claims that were abandoned: {e}"
        ))
    })?;
    let tags = tag_names(&listing.stdout_utf8());
    let shipped = |v: &str| tags.contains(&format!("v{v}"));
    match predecessor_release(&ledger_text, version, &shipped)? {
        Some(release) => handoff_fixtures_of(tree, &release).map(Some),
        None => Ok(None),
    }
}

/// The tag names in `git ls-remote --tags` output, with the peeled `^{}` rows
/// of annotated tags folded into their tag.
fn tag_names(listing: &str) -> std::collections::BTreeSet<String> {
    listing
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter_map(|refname| refname.strip_prefix("refs/tags/"))
        .map(|tag| tag.strip_suffix("^{}").unwrap_or(tag).to_string())
        .collect()
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
fn handoff_fixtures_of(tree: &GateTree<'_>, release: &str) -> Result<HandoffFixtures> {
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
    // THE DESK SET (the round-4 update audit, plan item 5). One pinned desk
    // used to pass, so a release whose generator wrote only `history` would
    // have let the cut after it through with most of its producer unchecked.
    // Each desk the guard requires must be pinned and on disk for every
    // release from ITS OWN floor on. Not "the previous release's desks":
    // v0.94.0 and v0.95.0 rightly lack `stalled-sequence`, which their
    // producers' generators could not make. And not one floor for the whole
    // set (the round-four review): a desk added for release N would then be
    // required of every frozen release before it, which can never gain it.
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
fn pinned_desks(guard: &str) -> Result<Vec<(String, String)>> {
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
struct RequiredDesk {
    /// The first release held to it, as the ledger spells a version
    /// (`0.97.0`, the guard's `v` dropped).
    from: String,
    desk: String,
}

/// The guard's `REQUIRED_DESKS` table — `("vX.Y.Z", "<desk>")` rows, each desk
/// with its own floor — read from its source like [`pinned_desks`] and failing
/// closed like it: a renamed constant, an unpaired row, an empty table, or a
/// floor that is not a version, stops the cut.
fn required_desks(guard: &str) -> Result<Vec<RequiredDesk>> {
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
fn release_at_least(release: &str, floor: &str) -> Result<bool> {
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

/// RUN THE CROSS-VERSION GUARD IN THE CUT TREE, before the claim (the round-4
/// update audit, plan item 5).
///
/// [`handoff_fixture_gate`] proves the predecessor's fixtures are checked in
/// and pinned. It cannot prove this build's consumer adopts them: that is
/// what the guard in [`HANDOFF_FIXTURE_GUARD`] runs, and nothing made a cut
/// run it — a consumer change that refused a shipped producer's bytes, landed
/// without the test suite, would ship, and every installed copy of that
/// producer would stay on it. So the cut runs it, in the tree it is about to
/// build: `targo --unverified test -p aterm-gui --lib` filtered to
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

/// [`handoff_fixture_guard_gate`] with the `targo` it runs named, so a test
/// needs no toolchain.
fn handoff_fixture_guard_with(
    targo: &Path,
    tree: &Path,
    run: &mut dyn FnMut(&mut Command) -> std::io::Result<std::process::Output>,
) -> Result<usize> {
    let target = tree.join(HANDOFF_FIXTURE_GUARD_TARGET);
    let clear = |target: &Path| match fs::symlink_metadata(target) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(target),
        Ok(_) => fs::remove_file(target),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    };
    clear(&target).map_err(|e| {
        Error::new(format!(
            "handoff fixture guard: cannot clear its target directory {}: {e}",
            target.display()
        ))
    })?;
    println!(
        "==> handoff fixture guard: every shipped producer's desks through this build's \
         consumer (targo --unverified test -p aterm-gui --lib -- {HANDOFF_FIXTURE_GUARD_FILTER})"
    );
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
        .arg(&target)
        .args(["--", HANDOFF_FIXTURE_GUARD_FILTER])
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
    let outcome = run(&mut command);
    // Removed whatever the outcome; a failure to remove is said, never a refusal
    // of its own — the verdict is the guard's.
    if let Err(error) = clear(&target) {
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

/// Read the guard's libtest output: how many tests passed, or the refusal —
/// for a failed run (naming the failing tests, or the build's last error
/// lines when it never ran a test) and for a run in which no test ran.
fn fixture_guard_verdict(success: bool, stdout: &str, stderr: &str) -> Result<usize> {
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
    let refuse = |why: String| {
        Error::new(format!(
            "handoff fixture guard: {why}\n\
             the guard adopts every shipped release's recorded handoff desks with the \
             consumer this cut ships; a red guard means an installed release could not \
             hand its sessions to this build\n\
             fix:  run `targo --unverified test -p aterm-gui --lib -- \
             {HANDOFF_FIXTURE_GUARD_FILTER}` and fix the CONSUMER, never a fixture \
             ({HANDOFF_FIXTURE_README})\n\
             (refused before the claim: no build number was spent)"
        ))
    };
    if !success || failed > 0 {
        let failing: Vec<&str> = stdout
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("test ")
                    .and_then(|rest| rest.strip_suffix(" ... FAILED"))
            })
            .collect();
        let why = if failing.is_empty() {
            let tail: Vec<&str> = stderr
                .lines()
                .filter(|line| line.starts_with("error"))
                .take(5)
                .collect();
            format!(
                "the guard did not pass, and no test failed by name — the build or the run \
                 stopped first:\n{}",
                if tail.is_empty() {
                    "(no error line)".to_string()
                } else {
                    tail.join("\n")
                }
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
/// what is missing, why the cut cannot go without it, and where the steps are.
fn handoff_refusal(release: &str, missing: &str) -> Error {
    Error::new(format!(
        "the handoff fixtures of v{release}, the release this cut succeeds, are not checked in: \
         {missing}\n\
         an installed v{release} updates by handing its sessions to the new build, and \
         nothing checks that this build adopts what v{release} writes until they are\n\
         fix:  generate them in a worktree at tag v{release} and pin their rows in \
         PINNED_DESKS — the steps are in {HANDOFF_FIXTURE_README}\n\
         (refused before the claim: no build number was spent)"
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
/// ecb1d6691 (2026-08-30) replaced the two per-triple copies with one
/// `[target.'cfg(trust_verify)']` table scoped to the COMPILER that understands
/// the flag rather than to a host, so the single table reaches every Trust lane
/// on every target. The per-triple names are retired with them: the cutter
/// keeps no reader for a config shape this tree no longer carries, and
/// `the_gate_knows_the_name_of_the_table_the_config_actually_carries` goes red
/// the day the table moves again.
const TRUST_LANE_TABLES: [&str; 1] = ["cfg(trust_verify)"];

/// trustc's own words for "the off-switch took effect".
///
/// The gate reads THIS rather than matching a flag spelling, and the distinction
/// is the point: hardcoding `-Ztrust-verify=off` anywhere in the cutter is the
/// drift [`native_lane_rustflags`] exists to avoid, so the gate asks the compiler
/// whether the config's flags — whatever they spell — actually turned
/// verification off. Measured 2026-09-16 against a stage2 trustc compiling
/// [`PROBE_SRC`] with `--emit=metadata`: under the config's flags,
/// `note: compiled WITHOUT Trust verification — 0 obligations checked`; under no
/// flags, `=== Trust Verification Report (probe) ===` and a
/// `note: Trust verification: 1 proved, … out of 1 obligation(s)` instead.
const OFF_SWITCH_NOTE: &str = "compiled WITHOUT Trust verification";

/// The probe's source. It carries ONE real Level 0 obligation on purpose — the
/// divisor-is-zero check on `numerator / denominator`.
///
/// `fn main() {}` used to stand here and it made the probe unfalsifiable. A
/// program with no obligations compiles the same way in both lanes and trustc
/// says "0 obligations checked" either way, so NOTHING about the verification
/// lane is observable in its output — the probe could only ever see a broken
/// stage2 or a flag spelling the compiler cannot parse. One obligation is enough
/// to make the two lanes print different things, which is what lets
/// [`verification_off_verdict`] tell them apart.
const PROBE_SRC: &str = "\
pub fn divide(numerator: u32, denominator: u32) -> u32 {
    numerator / denominator
}

fn main() {
    let _ = divide(6, 3);
}
";

/// The native-lane rustflags the repo's `.cargo/config.toml` applies to the
/// Trust slice — the ONE temporary verification opt-out. Read from the file,
/// never hardcoded: a hardcoded off-switch spelling drifted from the config
/// twice (`-Zno-trust-verify=yes` vs `-Ztrust-verify=off`) and either direction
/// of that drift kills the cut in the build step.
///
/// FAILS CLOSED when the file is there and none of [`TRUST_LANE_TABLES`] is. It
/// used to answer `[]` for that case and read the emptiness as good news — "the
/// table is gone, the Trust-Std campaign greened, so the probe compiles
/// batteries-on like the build lane will". Through a single dead key a RENAMED
/// table is indistinguishable from a deleted one, and the rename is what
/// actually happened: ecb1d6691 moved the table on 2026-08-30 while this reader
/// went on asking for `aarch64-apple-darwin`, so from that day the macOS release
/// gate probed with no flags at all and could not fail on them — and the
/// campaign's own file still says it is red (`targo trust check -p aterm-types`,
/// 708 errors, measured 2026-09-16). The campaign greening is a deliberate edit
/// here, never something a lookup miss may assume on the reader's behalf.
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
             [target] has a `rustflags` array. The probe compiles under the flags the \
             build lane will really use, so a table renamed out from under this list \
             makes the probe silently flagless — which is exactly how this gate stopped \
             gating on 2026-08-30. If the table moved, add its name to \
             TRUST_LANE_TABLES; if the Trust-Std campaign greened and the opt-out is \
             genuinely gone, say so here rather than letting a lookup miss say it.",
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

/// Did the config's flags actually turn verification off in the trustc that just
/// ran? Pure over (flags, trustc's stderr), so the verdict is a test rather than
/// a promise.
///
/// Three drifts die here, all of them at the cheap pre-claim moment instead of
/// twenty minutes into the build step:
///
/// * the flag list arrived EMPTY — the Trust-lane table renamed away again, or a
///   `.cargo/config.toml` that is not this repo's;
/// * the switch parsed but did not switch (a value drift, `…=off` to `…=on`):
///   trustc prints its verification report instead of [`OFF_SWITCH_NOTE`];
/// * the off-switch left the table while the campaign is still red.
///
/// A spelling the compiler cannot parse never reaches here — that one fails the
/// compile itself, one branch above this call.
fn verification_off_verdict(flags: &[String], stderr: &str) -> Result<()> {
    if flags.is_empty() {
        return Err(Error::new(
            "the native lane's rustflags came back EMPTY, so the trustc probe compiled \
             under no flags and gated on nothing. .cargo/config.toml's Trust-lane table \
             is the source of truth for them (see native_lane_rustflags); a probe with \
             no flags cannot see a flag drift, which is the one thing it is for",
        ));
    }
    if !stderr.contains(OFF_SWITCH_NOTE) {
        return Err(Error::new(format!(
            "the probe compiled under the config's native-lane rustflags {flags:?} and \
             trustc did NOT report \"{OFF_SWITCH_NOTE}\" — the off-switch was accepted \
             but did not switch verification off, so the real build verifies every unit \
             strictly and dies deep in the build step. `trustc -Z help | grep \
             trust-verify` names the spelling this compiler accepts, and \
             .cargo/config.toml's Trust-lane table carries the one in use. trustc said: \
             {}",
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
/// The probe COMPILES [`PROBE_SRC`] under the exact rustflags
/// .cargo/config.toml applies to the native lane, not just `--version`: a
/// stage2 whose library build never landed has a runnable trustc but no std
/// rlibs in its sysroot (the 2026-07-07 dry-run failure — every crate E0463s
/// twenty seconds into the real build), and an off-switch spelling this trustc
/// does not parse fails every unit the same way. Compiling is the only honest
/// check that the toolchain can do what the build lane is about to ask of it;
/// the metadata-only emit keeps it fast (no codegen, no link).
///
/// Then it reads what trustc SAID, through [`verification_off_verdict`]. A
/// compile that succeeds proves the toolchain works; only the compiler's own
/// note proves the config's opt-out reached it. An exit status alone cannot,
/// which is why [`PROBE_SRC`] carries an obligation: the two lanes have to
/// differ before there is anything for a gate to read.
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
                 or from source rebuild the stage2 (`python3 x.py build --stage 2` in $HOME/trust), \
                 or point TRUST_STAGE2_BIN at a stage2 bin dir that carries trustc",
                trustc.display()
            )));
        }
    }
    // Sysroot smoke-compile under the exact native-lane flags the config applies.
    let flags = native_lane_rustflags(repo)?;
    let dir = std::env::temp_dir().join(format!("aterm-trust-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| Error::new(format!("probe tmpdir: {e}")))?;
    let src = dir.join("probe.rs");
    std::fs::write(&src, PROBE_SRC).map_err(|e| Error::new(format!("probe src: {e}")))?;
    let out = Command::new(&trustc)
        .args(&flags)
        .arg("--emit=metadata")
        .arg("--out-dir")
        .arg(&dir)
        .arg(&src)
        .output();
    let result = match out {
        Ok(o) if o.status.success() => {
            verification_off_verdict(&flags, &String::from_utf8_lossy(&o.stderr))
                .map(|()| trustc.clone())
        }
        Ok(o) => Err(Error::new(format!(
            "trustc at {} runs but cannot COMPILE under the native-lane rustflags {flags:?} \
             (stage2 library missing/stale — reinstall with `aterm pkg install trust`, or from \
             source rebuild with `python3 x.py build --stage 2` in $HOME/trust — or the config's \
             off-switch spelling does not match this trustc; `trustc -Z help | grep \
             trust-verify` decides): {}",
            trustc.display(),
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

/// The scheduling tier a launchd job was spawned at, as `launchctl print` names
/// it. `None` is "this launchd does not say", which is not a refusal.
#[derive(Debug, PartialEq, Eq)]
pub enum SpawnTier {
    /// Not a launchd job at all — an interactive shell, which is the tier the
    /// paint smoke was calibrated in.
    Shell,
    /// `spawn type = interactive (4)`: the tier a `ProcessType=Interactive`
    /// LaunchAgent gets, and the only launchd tier the smoke survives.
    Interactive,
    /// Any other named tier — `launchctl submit` produces this one.
    Other(String),
    /// A launchd job whose tier could not be read.
    Unknown,
}

/// Refuse, BEFORE the claim, a cut whose PAINT SMOKE is going to fail for the
/// scheduler's reasons rather than the renderer's.
///
/// Measured 2026-09-12 and recorded in docs/RELEASING.md: a job started with
/// `launchctl submit` runs at `QOS_CLASS_UTILITY`, its 50 ms sampling timer
/// fires 75 ms late on the first tick and 25-35 ms late at the median, and the
/// aterm instance the smoke launches INHERITS the tier. Thirty takes of the
/// cutter's exact smoke under `launchctl submit` went red eleven times; eight
/// from a shell went red none. The smoke runs at the self-check step — AFTER the
/// ledger claim — so that coin flip costs a burned build number each time it
/// lands tails, and `--resume` re-enters at the same starved step.
///
/// The tier is knowable before any of it: a launchd job's own label is in
/// `XPC_SERVICE_NAME`, and `launchctl print` names its `spawn type`. A cut from a
/// shell is the calibrated case and asks launchd nothing.
///
/// "Cannot tell" is a NOTE here, not a refusal — deliberately the opposite of
/// [`cutter_identity_gate`]. A wrong answer there ships a bad artifact; a wrong
/// answer here costs a flake on a resumable step, so an unreadable `launchctl
/// print` (an older macOS, a renamed field) must not be able to block a release.
pub fn launchd_qos_gate(paint_smoke_will_run: bool, rerun: &Rerun) -> Result<()> {
    let tier = match env::var("XPC_SERVICE_NAME").ok().filter(|l| l != "0") {
        None => SpawnTier::Shell,
        Some(label) => read_spawn_tier(&label),
    };
    match launchd_qos_verdict(&tier, paint_smoke_will_run, rerun)? {
        Some(note) => {
            println!("==> {note}");
            Ok(())
        }
        None => Ok(()),
    }
}

/// Ask launchd what tier it spawned this job at. Never fails: every unreadable
/// answer is [`SpawnTier::Unknown`].
fn read_spawn_tier(label: &str) -> SpawnTier {
    let uid = Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string());
    let Some(uid) = uid else {
        return SpawnTier::Unknown;
    };
    let out = Command::new("launchctl")
        .arg("print")
        .arg(format!("gui/{uid}/{label}"))
        .output();
    let Ok(out) = out else {
        return SpawnTier::Unknown;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("spawn type = ") {
            // "interactive (4)" — the tier name is the first word.
            let name = rest.split_whitespace().next().unwrap_or("").to_string();
            return if name == "interactive" {
                SpawnTier::Interactive
            } else if name.is_empty() {
                SpawnTier::Unknown
            } else {
                SpawnTier::Other(name)
            };
        }
    }
    SpawnTier::Unknown
}

/// The decision, without the environment. `Ok(None)` is silence, `Ok(Some(note))`
/// is something the transcript should say, `Err` is the refusal, whose fix is
/// `rerun`'s launcher command.
pub fn launchd_qos_verdict(
    tier: &SpawnTier,
    paint_smoke_will_run: bool,
    rerun: &Rerun,
) -> Result<Option<String>> {
    if !paint_smoke_will_run {
        // No smoke, no starvation to worry about: --no-paint-smoke already
        // carries its own (much louder) refusal on a notarized real cut.
        return Ok(None);
    }
    match tier {
        SpawnTier::Shell | SpawnTier::Interactive => Ok(None),
        SpawnTier::Unknown => Ok(Some(
            "scheduler tier: this cut is a launchd job whose spawn type launchd would not \
             name — if its plist does not say ProcessType=Interactive, the paint smoke will \
             be sampled at UTILITY and can fail for the scheduler's reasons (docs/RELEASING.md)"
                .to_string(),
        )),
        // The launcher bootstraps the ProcessType=Interactive agent itself. At a lower
        // tier the paint smoke's 50 ms timer fires 25-75 ms late, and 11 of 30 takes went
        // red for that alone, after the claim (docs/RELEASING.md, 2026-09-12).
        SpawnTier::Other(name) => Err(Error::new(format!(
            "this cut runs at launchd tier {name:?}, which starves the paint smoke; nothing \
             was claimed\n\
             fix:  {}",
            rerun.command
        ))),
    }
}

/// The provenance gate: refuse, BEFORE the claim, a cut whose files would all carry
/// `com.apple.provenance`.
///
/// macOS stamps that attribute on every file a provenance-TRACKED process writes, and a
/// process is tracked when its executable carries the tag or its parent is tracked
/// (measured 2026-09-12; `atpkg::provenance` has the table). Three things can bring it
/// into a cut, and this gate checks all three, the way the incident taught:
///
/// * the `trustc` the native slice will run — [`trustc_probe`] proved it compiles; its
///   attribute decides whether every object file, the proof snapshot and the release
///   artifacts inherit the tag (v0.83.0: the store's `trust/8590` had been seeded from a
///   tracked shell, and the cut died at tools/proof_snapshot.py's "published proof
///   snapshot has extended metadata" AFTER the ledger claim — a burned build number);
/// * the `targo` beside it, which drives that trustc and writes on its own;
/// * the dynamic libraries under the bundle's `lib/` that trustc loads — a tagged one
///   tracks the process that loads it (measured 2026-09-15), so they carry the tag into
///   a cut exactly as a tagged `trustc` does;
/// * the cutter's own binary — AND, separately, whether this PROCESS is tracked, which
///   the binary's attribute cannot tell (a clean cutter under a tracked parent — a shell
///   inside aterm.app, an agent started from a tagged `claude` — is tracked too). That is
///   MEASURED by writing a probe file and reading the attribute back; nothing this gate
///   could do would untrack a running process, and it says what does.
///
/// THE TOOLCHAIN IS HEALED FIRST ([`heal_toolchain`]): a tagged trustc, targo, dylib or
/// cutter binary is cleared in place by the same launchd job `aterm pkg` uses
/// (`atpkg::provenance::heal`, the one cure), and only what is STILL tagged afterwards is
/// refused — before the claim, so a refusal burns no build number. The heal cannot untrack
/// a running process: a cutter whose own binary was tagged is tracked until it is run again,
/// and the probe below says so.
///
/// A path this gate cannot inspect is a refusal, not a pass: the tag's whole failure mode
/// is being invisible until after the claim.
pub fn provenance_gate(trustc: &Path, rerun: &Rerun) -> Result<()> {
    provenance_gate_with(trustc, atpkg::provenance::heal, rerun)
}

/// [`provenance_gate`] with the toolchain heal explicit ([`atpkg::provenance::Healer`]).
/// A cut passes `atpkg::provenance::heal`; a test that runs the gate over this machine's
/// INSTALLED toolchain passes one that changes nothing, so a test run never rewrites the
/// store (the heal itself is tested on a scratch toolchain).
pub fn provenance_gate_with(
    trustc: &Path,
    heal: atpkg::provenance::Healer,
    rerun: &Rerun,
) -> Result<()> {
    let stage2 = trust_stage2_bin()?;
    let cutter = env::current_exe().and_then(fs::canonicalize).map_err(|e| {
        Error::new(format!(
            "provenance gate: cannot resolve the cutter's own binary: {e}"
        ))
    })?;
    let healed = heal_toolchain(trustc, &stage2, &cutter, heal);
    let carriers = toolchain_carriers(
        trustc,
        &stage2,
        &cutter,
        atpkg::provenance::PROVENANCE_XATTR,
    )?;
    let scratch = env::temp_dir();
    let tracked = atpkg::provenance::measure_tracked(&scratch).ok_or_else(|| {
        Error::new(format!(
            "provenance gate: could not write a probe file under {} to measure whether this \
             cutter process is provenance-tracked",
            scratch.display()
        ))
    })?;
    provenance_verdict(&carriers, tracked, healed.why(), rerun)
}

/// The `(label, path)` pairs of the cut's toolchain that carry `attr` (production:
/// `com.apple.provenance`): `trustc`, the `targo` in `stage2`, the cutter's own binary, and
/// the dylibs under the bundle's `lib/` that trustc loads — a process that `dlopen`s a
/// tagged library becomes tracked (measured 2026-09-15), so a clean `trustc` over a tagged
/// `lib/` writes tagged files all the same. The first three dylibs are named, the count
/// says the rest. A file that cannot be inspected is an `Err`, never clean.
fn toolchain_carriers(
    trustc: &Path,
    stage2: &Path,
    cutter: &Path,
    attr: &str,
) -> Result<Vec<(&'static str, PathBuf)>> {
    let mut carriers: Vec<(&'static str, PathBuf)> = Vec::new();
    for (label, path) in [
        ("trustc", trustc.to_path_buf()),
        ("targo", stage2.join("targo")),
        ("the cutter's own binary", cutter.to_path_buf()),
    ] {
        match atpkg::provenance::xattr_names(&path) {
            Ok(names) if names.iter().any(|n| n == attr) => carriers.push((label, path)),
            Ok(_) => {}
            Err(e) => {
                return Err(Error::new(format!(
                    "provenance gate: cannot inspect {label} at {} for {attr}: {e}",
                    path.display()
                )));
            }
        }
    }
    if let Some(lib) = trustc
        .parent()
        .and_then(Path::parent)
        .map(|b| b.join("lib"))
        && lib.is_dir()
    {
        let dylibs: Vec<PathBuf> = atpkg::provenance::tagged_files_under(&lib, attr)
            .carriers
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "dylib"))
            .collect();
        for path in dylibs.iter().take(3) {
            carriers.push(("a library trustc loads", path.clone()));
        }
        if dylibs.len() > 3 {
            carriers.push((
                "…and more libraries under the bundle's lib/ (count in the message)",
                lib.join(format!("({} tagged dylibs in all)", dylibs.len())),
            ));
        }
    }
    Ok(carriers)
}

/// What [`heal_toolchain`] clears: the real `bin/` that holds `trustc`, the bundle's `lib/`
/// beside it (the dylibs trustc loads), the real directory holding `targo` when it is a
/// different one, and the cutter's own binary — a FILE, never the directory it sits in (a
/// cargo `target/`). Every path is resolved first: the store reaches the bundle through its
/// `current` link, and the heal never follows a link.
fn toolchain_heal_roots(trustc: &Path, stage2: &Path, cutter: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut add = |path: PathBuf| {
        if path.exists() && !roots.contains(&path) {
            roots.push(path);
        }
    };
    if let Some(bin) = fs::canonicalize(trustc)
        .ok()
        .and_then(|real| real.parent().map(Path::to_path_buf))
    {
        if let Some(bundle) = bin.parent() {
            add(bundle.join("lib"));
        }
        add(bin);
    }
    if let Some(targo_dir) = fs::canonicalize(stage2.join("targo"))
        .ok()
        .and_then(|real| real.parent().map(Path::to_path_buf))
    {
        add(targo_dir);
    }
    if let Ok(cutter) = fs::canonicalize(cutter) {
        add(cutter);
    }
    roots.sort();
    roots
}

/// Clear `com.apple.provenance` from the toolchain this cut is about to run and from the
/// cutter's own binary ([`toolchain_heal_roots`]) before [`provenance_gate`] reads them,
/// and say so on the transcript when it cleared something. `heal` is
/// `atpkg::provenance::heal` in a cut: one launchd job made only of platform binaries,
/// untracked whatever this process is, that removes that ONE attribute and re-measures. It
/// runs without the store lock — it changes no byte of any file, and the gate's own read
/// afterwards is the verdict, so a pass that moves the store underneath can only make it
/// refuse, never pass wrongly.
fn heal_toolchain(
    trustc: &Path,
    stage2: &Path,
    cutter: &Path,
    heal: atpkg::provenance::Healer,
) -> atpkg::provenance::HealOutcome {
    let outcome = heal(
        &toolchain_heal_roots(trustc, stage2, cutter),
        &env::temp_dir(),
    );
    if let atpkg::provenance::HealOutcome::Healed { cleared } = outcome {
        println!(
            "==> provenance gate: cleared com.apple.provenance from {} in the toolchain",
            atpkg::provenance::count_of(cleared, "file")
        );
    }
    outcome
}

/// The decision behind [`provenance_gate`], without the filesystem: `carriers` are the
/// `(label, path)` pairs found tagged, `cutter_tracked` is the probe-write measurement,
/// and `heal_left` is why the toolchain heal left files tagged, when it did. Clean on
/// both counts passes silently; anything else is the one refusal, naming every carrier,
/// what the tag does, and the ways out — the launcher spelled as `rerun` repeats this
/// run, and never offered to a cut that already runs under it.
pub fn provenance_verdict(
    carriers: &[(&str, PathBuf)],
    cutter_tracked: bool,
    heal_left: Option<&str>,
    rerun: &Rerun,
) -> Result<()> {
    if carriers.is_empty() && !cutter_tracked {
        return Ok(());
    }
    let mut msg = String::from(
        "com.apple.provenance would reach every file this cut writes — refusing BEFORE the \
         claim (after it, a build number is burned):\n",
    );
    for (label, path) in carriers {
        msg.push_str(&format!(
            "  {label}: {} carries com.apple.provenance\n",
            path.display()
        ));
    }
    if !carriers.is_empty()
        && let Some(why) = heal_left
    {
        msg.push_str(&format!(
            "  the gate cleared the toolchain and the cutter through a launchd job first, and \
             the tag stayed: {why}\n"
        ));
    }
    if cutter_tracked {
        msg.push_str(
            "  this cutter PROCESS is provenance-tracked: a probe file it wrote came back \
             tagged — its own binary carried the tag when it was started (cleared above, so a \
             fresh start can run clean), or it runs under a tracked parent (a shell inside a \
             tracked aterm.app, or an agent started from a tagged binary such as a natively \
             installed `claude`); a running process stays tracked, so even an untagged \
             toolchain would write tagged files from here\n",
        );
    }
    msg.push_str(atpkg::provenance::WHAT_IT_BREAKS);
    if carriers.is_empty() {
        // Only the PROCESS is tracked, and nothing clears a running process. Outside the
        // launcher, the launcher is the fix. Under it, the job is /bin/zsh exec'ing the
        // targo shim on PATH, which this gate neither reads nor clears: a tagged shim
        // tracks the job, and `aterm pkg repair` clears the store's bin/ shims.
        if rerun.under_launcher {
            msg.push_str(&format!(
                "\nfix:  aterm pkg repair   (it clears a tagged targo shim, which the job \
                 execs), then {}",
                rerun.command
            ));
        } else {
            msg.push_str(&format!(
                "\nfix:  {}   (runs the cut as a launchd job, untracked)",
                rerun.command
            ));
        }
        return Err(Error::new(msg));
    }
    msg.push_str("\nfix:  ");
    msg.push_str(atpkg::provenance::REMEDY);
    if rerun.under_launcher {
        return Err(Error::new(msg));
    }
    msg.push_str(&format!(
        // The remedy names the LAUNCHER, not the plist it writes. This message is
        // what sent every cut of this session to QOS_CLASS_UTILITY: it used to read
        // `launchctl submit`, an operator pasted exactly that, and the post-claim
        // paint smoke was starved by the tier it chose. A refusal that hands over a
        // runnable command is the whole fix — `tools/cut-launch.sh` bootstraps the
        // ProcessType=Interactive agent itself, so nobody has to transcribe a plist.
        "\nor:   with the toolchain clean, run this cut as a launchd job — `{}` — so the \
         cutter is neither a descendant of aterm.app nor of an agent. Use THAT launcher and \
         not `launchctl submit`: submit escapes this tag and runs the job at \
         QOS_CLASS_UTILITY, which the paint smoke's aterm inherits and starves under (11 of \
         30 takes red; docs/RELEASING.md)",
        rerun.command
    ));
    Err(Error::new(msg))
}

/// The x86_64 compat slice builds on upstream stable (buildplan.rs pins
/// `RUSTUP_TOOLCHAIN=stable`), so probe STABLE's installed targets — a bare
/// `rustup target list` would resolve the repo's `trust` toolchain via
/// rust-toolchain.toml, and custom toolchains never carry rustup-managed
/// targets. When the target is absent, print the exact remediation and
/// require the explicit `--arm64-only` to proceed single-arch (spec decision
/// 18) — never silently ship a thinner artifact than v0.25 did.
/// STOCK EXCEPTION (Trust lacks an x86_64-apple-darwin std; measured 2026-09-28
/// `targo --unverified check --target x86_64-apple-darwin -p aterm`: error[E0463]).
pub fn x86_target_probe() -> Result<()> {
    let out = Command::new("rustup")
        .env("RUSTUP_TOOLCHAIN", "stable")
        .args(["target", "list", "--installed"])
        .output()
        .map_err(|e| {
            // Name the escape hatch. rustup is NOT this repo's toolchain — THE
            // toolchain is the Trust stage2 tree — and it is wanted here for
            // exactly one thing: upstream stable's x86_64-apple-darwin std. The
            // installed host Trust sysroot carries only its host std
            // (rust-toolchain.toml; buildplan.rs says why the compat slice rides
            // stable). So on a Trust-only machine this is not a broken setup to go
            // fix; it is a choice about what to ship, and the operator needs to be
            // told that rather than sent to install a toolchain manager the repo
            // otherwise refuses.
            Error::new(format!(
                "failed to run rustup ({e}). The x86_64 compat slice needs upstream \
                 stable's std for that target (the installed host Trust sysroot carries \
                 only its host std — the one documented \
                 exception to the single-Trust lane). Either install rustup and run \
                 `rustup +stable target add x86_64-apple-darwin`, or pass --arm64-only \
                 to ship an Apple-Silicon-only build deliberately."
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

/// Whether this cut ships a universal binary, and — when it does — the proof, BEFORE the
/// claim, that this builder can both BUILD the x86_64 slice (`target`, the stable
/// `x86_64-apple-darwin` std) and RUN it (`rosetta`). The build lane runs the slice under
/// Rosetta after it is linked (buildplan.rs, docs/DESIGN-intel-just-works-2026-09-14.md
/// §3.1) — past `ledger::claim`, which has already pushed the release commit and taken a
/// build number. A builder without Rosetta found out THERE used to burn that number: the
/// cut's refusals fire pre-claim so a refused cut consumes nothing and retries freely.
/// `--arm64-only` asks neither probe. Both probes are injected so every arm is testable
/// on a machine that has Rosetta.
pub(crate) fn universal_gate(
    arm64_only: bool,
    target: impl FnOnce() -> Result<()>,
    rosetta: impl FnOnce() -> std::result::Result<(), String>,
) -> Result<bool> {
    if arm64_only {
        return Ok(false);
    }
    // The cut appends the remedies itself: the probe returns a fault, and this is the only
    // caller that must STOP, so it is the only one that has to say what to do about it.
    target().map_err(|e| {
        Error::new(format!(
            "{e}\nfix:  {}",
            X86_SLICE_REMEDIES.replace('\n', "\n      ")
        ))
    })?;
    rosetta().map_err(Error::new)?;
    Ok(true)
}

/// The refusal a universal cut gets on a builder that cannot RUN its x86_64 slice.
pub(crate) const ROSETTA_REFUSAL: &str = "the universal cut RUNS its x86_64 slice under Rosetta before it \
     ships (an unexecuted slice is how every Intel-only fault reached a release), and Rosetta \
     does not run here — softwareupdate --install-rosetta --agree-to-license, or pass \
     --arm64-only to ship an Apple-Silicon-only build deliberately";

/// Whether Rosetta runs a trivial x86_64 program here (`/usr/bin/arch -x86_64
/// /usr/bin/true`). `run` spawns, injected so the refusal is testable on a machine that
/// has Rosetta. The ONE Rosetta probe: the pre-claim [`universal_gate`], the provision
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
            "only {free_gib} GiB free — a universal release build needs at least \
             {MIN_FREE_DISK_GIB} GiB (two release target trees + dist/ staging)"
        )));
    }
    Ok(free_gib)
}

#[cfg(test)]
mod universal_gate_tests {
    use std::cell::Cell;
    use std::os::unix::process::ExitStatusExt as _;

    use super::*;

    fn exited(code: i32) -> std::process::Output {
        std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: b"bad CPU type in executable".to_vec(),
        }
    }

    /// A builder without Rosetta is refused by the PRE-CLAIM gate, naming the remedy and
    /// the escape — it used to learn it inside `buildplan::run`, after `ledger::claim` had
    /// pushed the release commit and taken a build number.
    #[test]
    fn no_rosetta_is_refused_before_the_claim() {
        let err = universal_gate(false, || Ok(()), || Err(ROSETTA_REFUSAL.to_string()))
            .expect_err("no Rosetta must refuse a universal cut");
        assert_eq!(err.to_string(), ROSETTA_REFUSAL);
        assert!(
            ROSETTA_REFUSAL.contains("softwareupdate --install-rosetta")
                && ROSETTA_REFUSAL.contains("--arm64-only")
        );
    }

    /// Each arm asks only what it needs: `--arm64-only` neither probe; a missing stable
    /// target stops before Rosetta is asked, with the target remedies laid out; both
    /// present is a universal cut.
    #[test]
    fn each_arm_asks_only_the_probes_it_needs() {
        let asked = Cell::new(0);
        let probe = || {
            asked.set(asked.get() + 1);
            Ok(())
        };
        assert!(
            !universal_gate(true, probe, || unreachable!("arm64-only asks no Rosetta")).unwrap()
        );
        assert_eq!(
            asked.get(),
            0,
            "--arm64-only must not probe the x86_64 target"
        );

        let err = universal_gate(
            false,
            || Err(Error::new("x86_64-apple-darwin target missing")),
            || unreachable!("a missing target stops before Rosetta is asked"),
        )
        .expect_err("a missing target refuses");
        let err = err.to_string();
        assert!(
            err.contains("target missing") && err.contains("fix:  rustup +stable target add"),
            "{err}"
        );

        assert!(universal_gate(false, || Ok(()), || Ok(())).unwrap());
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

    /// Bound to the real gate: `run_all` — which `publish.rs` runs before `ledger::claim` —
    /// decides universality through `universal_gate` with the REAL Rosetta probe. Asserted as
    /// source because the alternative is a builder without Rosetta.
    #[test]
    fn the_pre_claim_gate_asks_rosetta() {
        let gates = include_str!("gates.rs");
        let body = &gates[gates.find("pub fn run_all(").expect("run_all")..];
        let body = &body[..body.find("\n}\n").expect("end of run_all")];
        // Split so these assertions do not match themselves.
        let decided = body
            .find(concat!(
                "universal_gate(",
                "opts.arm64_only, x86_target_probe"
            ))
            .expect("run_all decides universality through universal_gate");
        assert!(
            body[decided..].contains(concat!("rosetta_runs(", "&mut |command| command.output())")),
            "run_all must hand universal_gate the real Rosetta probe"
        );
        let publish = include_str!("publish.rs");
        let gated = publish
            .find("gates::run_all(")
            .expect("the cut runs the gates");
        let claimed = publish.find("ledger::claim(").expect("the cut claims");
        assert!(gated < claimed, "the gates must run before the claim");
    }
}

#[cfg(test)]
mod cut_lease_tests {
    use super::*;

    /// A CUT LEASES THE TOOLCHAIN IT PINNED (2026-09-26): under the store's prefix, a
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
mod launchd_qos_tests {
    use super::*;
    use crate::publish::CutKind;

    fn rerun(kind: CutKind) -> Rerun {
        Rerun {
            under_launcher: false,
            ..Rerun::of(kind, Some("me/scratch"))
        }
    }

    #[test]
    fn a_shell_and_an_interactive_agent_are_the_calibrated_cases() {
        for tier in [SpawnTier::Shell, SpawnTier::Interactive] {
            assert_eq!(
                launchd_qos_verdict(&tier, true, &rerun(CutKind::Real))
                    .expect("calibrated tiers pass"),
                None,
                "{tier:?} must pass silently"
            );
        }
    }

    /// `launchctl submit`'s tier, refused BEFORE the claim rather than sampled
    /// after it.
    #[test]
    fn a_utility_job_is_refused_with_the_launcher_remedy() {
        let err = launchd_qos_verdict(
            &SpawnTier::Other("background".to_string()),
            true,
            &rerun(CutKind::Real),
        )
        .expect_err("a starved tier must not reach the claim");
        let msg = err.to_string();
        assert!(msg.contains("\"background\""), "it names the tier: {msg}");
        assert!(msg.contains("nothing was claimed"), "{msg}");
        assert!(
            msg.contains("fix:  tools/cut-launch.sh --release-credentials <path>"),
            "the one remedy is the launcher, which sets the tier itself: {msg}"
        );
        assert!(
            !msg.contains("interactive shell"),
            "a shell under aterm.app is refused by the provenance gate: {msg}"
        );
    }

    /// The fix repeats THIS run: copied from a dry run's or a rehearsal's refusal, it
    /// must not start a real cut, which claims a build number and publishes.
    #[test]
    fn the_launcher_remedy_keeps_the_runs_own_flags() {
        for (kind, spelled) in [
            (
                CutKind::DryRun,
                "fix:  tools/cut-launch.sh --dry-run --release-credentials <path>",
            ),
            (
                CutKind::Rehearse,
                "fix:  tools/cut-launch.sh --rehearse me/scratch --release-credentials <path>",
            ),
        ] {
            let msg = launchd_qos_verdict(
                &SpawnTier::Other("background".to_string()),
                true,
                &rerun(kind),
            )
            .expect_err("a starved tier must not reach the claim")
            .to_string();
            assert!(msg.ends_with(spelled), "{kind:?}: {msg}");
        }
    }

    /// Cannot-tell is a NOTE, not a refusal — the opposite of the identity gate,
    /// and deliberately: the cost here is a resumable flake, not a bad artifact.
    #[test]
    fn an_unreadable_tier_notes_and_proceeds() {
        let note = launchd_qos_verdict(&SpawnTier::Unknown, true, &rerun(CutKind::Real))
            .expect("cannot-tell must not block a release")
            .expect("but it must say something");
        assert!(note.contains("ProcessType=Interactive"), "{note}");
    }

    /// No smoke, nothing to starve.
    #[test]
    fn no_paint_smoke_makes_the_tier_irrelevant() {
        assert_eq!(
            launchd_qos_verdict(
                &SpawnTier::Other("background".to_string()),
                false,
                &rerun(CutKind::Real)
            )
            .expect("nothing to starve"),
            None
        );
    }
}

#[cfg(test)]
mod closure_tests {
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
        fn saw(&self, needle: &str) -> bool {
            self.calls
                .lock()
                .expect("calls")
                .iter()
                .any(|c| c.contains(needle))
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

    const HEAD: &str = "1111111111111111111111111111111111111111";
    const TIP: &str = "2222222222222222222222222222222222222222";

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

    #[test]
    fn a_closure_untouched_by_the_diff_is_identical() {
        let git = FakeGit::new(vec![
            ("cat-file -e", out(0, "")),
            ("merge-base --is-ancestor", out(0, "")),
            ("diff --name-only", out(0, "")),
        ]);
        assert_eq!(
            cutter_source_closure(&git, HEAD, TIP),
            SourceClosure::Identical
        );
        // The diff must be SCOPED — an unscoped one would call every peer push a
        // change to the cutter and hand the race straight back.
        assert!(
            git.saw("-- crates vendor"),
            "the diff must name SOURCE_INPUTS"
        );
    }

    #[test]
    fn a_stamp_this_checkout_does_not_have_is_unresolvable() {
        let git = FakeGit::new(vec![("cat-file -e", out(1, ""))]);
        assert!(matches!(
            cutter_source_closure(&git, HEAD, TIP),
            SourceClosure::Unresolvable(_)
        ));
    }

    /// Provenance, not coincidence: a binary from a foreign branch whose files
    /// happen to match is still not this tree's cutter. BOTH directions must be
    /// asked and refused — a missing answer defaults to success here.
    #[test]
    fn a_stamp_off_this_line_of_history_is_unresolvable() {
        let git = FakeGit::new(vec![
            ("cat-file -e", out(0, "")),
            ("merge-base --is-ancestor 1111", out(1, "")),
            ("merge-base --is-ancestor 2222", out(1, "")),
        ]);
        match cutter_source_closure(&git, HEAD, TIP) {
            SourceClosure::Unresolvable(why) => assert!(why.contains("one line"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert!(
            !git.saw("diff --name-only"),
            "no diff is taken off the line"
        );
    }

    /// A real cut builds the published commit, usually OLDER than the tip the
    /// cutter compiled at (2026-09-23): a stamp that DESCENDS from the head is the
    /// same line of history, and an untouched closure makes it that head's cutter.
    /// Before, only a stamp that was an ancestor of HEAD counted, so every such cut
    /// would have been refused.
    #[test]
    fn a_stamp_that_descends_from_the_head_is_the_same_line() {
        let git = FakeGit::new(vec![
            ("cat-file -e", out(0, "")),
            // stamp (1111) is NOT an ancestor of head (2222)…
            ("merge-base --is-ancestor 1111", out(1, "")),
            // …but head is an ancestor of the stamp.
            ("merge-base --is-ancestor 2222", out(0, "")),
            ("diff --name-only", out(0, "")),
        ]);
        assert_eq!(
            cutter_source_closure(&git, HEAD, TIP),
            SourceClosure::Identical
        );
        assert!(git.saw("merge-base --is-ancestor 2222222222222222222222222222222222222222 1111"));
    }

    /// The list in this module is a copy of the one `build.rs` watches and judges
    /// dirty. Copies drift; this refuses the drift instead of trusting a comment.
    #[test]
    fn the_source_closure_matches_the_one_build_rs_judges() {
        let build_rs = include_str!("../build.rs");
        let start = build_rs
            .find("const SOURCE_INPUTS: &[&str] = &[")
            .expect("build.rs declares SOURCE_INPUTS");
        let body = &build_rs[start..];
        let body = &body[..body.find("];").expect("terminated array")];
        let declared: Vec<String> = body
            .lines()
            .filter_map(|l| l.trim().strip_prefix('"'))
            .filter_map(|l| l.split('"').next())
            .map(str::to_string)
            .collect();
        assert_eq!(
            declared,
            SOURCE_INPUTS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            "gates::SOURCE_INPUTS and build.rs's SOURCE_INPUTS have drifted — the identity \
             gate would then admit a binary built from source it cannot see"
        );
    }
}

#[cfg(test)]
mod cutter_identity_tests {
    use super::*;

    const HEAD: &str = "2617295d1111111111111111111111111111aaaa";
    const OLDER: &str = "15f2b5b6bc51d9ef56e5e4fa50f434fca07f40ee";

    /// The closure verdict every pre-existing case was written under: the stamp
    /// and HEAD differ AND so does the cutter's own source.
    fn moved() -> SourceClosure {
        SourceClosure::Changed(vec!["crates/aterm-release/src/publish.rs".to_string()])
    }

    #[test]
    fn the_trees_own_binary_passes_silently() {
        assert!(matches!(
            cutter_identity_verdict(HEAD, HEAD, &moved(), false),
            Ok(None)
        ));
    }

    /// The v0.63.0 shape: a real cut by a binary older than its own source.
    #[test]
    fn a_stale_binary_cannot_cut_for_real() {
        let err = cutter_identity_verdict(OLDER, HEAD, &moved(), false)
            .expect_err("a stale cutter must not cut a real release");
        let msg = err.to_string();
        assert!(msg.contains(OLDER), "{msg}");
        assert!(msg.contains(HEAD), "{msg}");
        assert!(
            msg.contains("targo clean --release -p aterm-release"),
            "{msg}"
        );
        assert!(
            msg.contains("crates/aterm-release/src/publish.rs"),
            "the refusal names what moved: {msg}"
        );
        assert!(
            msg.contains("v0.63.0"),
            "the refusal should say why it exists: {msg}"
        );
    }

    /// "Cannot tell" must refuse, not pass — an unstamped binary is exactly as
    /// unproven as a stale one.
    #[test]
    fn an_unstamped_binary_fails_closed() {
        let err = cutter_identity_verdict("unknown", HEAD, &moved(), false)
            .expect_err("an unstamped cutter must fail closed");
        assert!(err.to_string().contains("no build commit"), "{err}");
    }

    #[test]
    fn a_dirty_built_binary_fails_closed() {
        let err = cutter_identity_verdict(DIRTY_BUILD_COMMIT, HEAD, &moved(), false)
            .expect_err("uncommitted build-time code must never publish after the tree is clean");
        assert!(
            err.to_string().contains("dirty repository source closure"),
            "{err}"
        );
    }

    /// A dry-run publishes nowhere, so it proceeds — but says so, because its
    /// results came from code other than the tree.
    #[test]
    fn a_dry_run_proceeds_but_says_so() {
        let note = cutter_identity_verdict(OLDER, HEAD, &moved(), true)
            .expect("a dry-run must not be blocked")
            .expect("a mismatched dry-run must say something");
        assert!(note.contains("dry-run publishes nowhere"), "{note}");
        assert!(note.contains("OTHER code"), "{note}");
        assert!(
            cutter_identity_verdict("unknown", HEAD, &moved(), true).is_ok(),
            "an unstamped dry-run is not blocked either"
        );
        // ...and a matching dry-run stays silent.
        assert!(matches!(
            cutter_identity_verdict(HEAD, HEAD, &moved(), true),
            Ok(None)
        ));
    }

    #[test]
    fn a_stale_rehearsal_is_refused_because_it_mutates_a_remote() {
        let err = cutter_identity_verdict(OLDER, HEAD, &moved(), false)
            .expect_err("a scratch-repository publication still needs the tree's own cutter");
        assert!(err.to_string().contains(OLDER), "{err}");
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

    /// THE RACE THIS GATE USED TO LOSE. A peer pushes to another program while
    /// the cutter is compiling; HEAD moves; not one byte the binary was built
    /// from moved with it. The binary IS what this tree builds, so it cuts —
    /// and says which two commits it reconciled.
    #[test]
    fn a_peer_push_that_misses_the_cutters_source_does_not_stale_it() {
        let note = cutter_identity_verdict(OLDER, HEAD, &SourceClosure::Identical, false)
            .expect("an unchanged source closure is not a stale cutter")
            .expect("it must say what it reconciled");
        assert!(note.contains(OLDER), "{note}");
        assert!(note.contains(HEAD), "{note}");
        assert!(note.contains("source closure"), "{note}");
    }

    /// ...but an unstamped or dirty binary is never rescued by a closure answer:
    /// there is no commit to compare, so `Identical` cannot be meant about it.
    #[test]
    fn the_closure_escape_needs_a_real_stamp() {
        for stamp in ["unknown", DIRTY_BUILD_COMMIT] {
            assert!(
                cutter_identity_verdict(stamp, HEAD, &SourceClosure::Identical, false).is_err(),
                "{stamp} must fail closed whatever the closure says"
            );
        }
    }

    /// An unresolvable closure refuses and says it could not tell — never the
    /// permissive answer.
    #[test]
    fn an_unresolvable_closure_refuses_and_says_so() {
        let err = cutter_identity_verdict(
            OLDER,
            HEAD,
            &SourceClosure::Unresolvable("not in this checkout".to_string()),
            false,
        )
        .expect_err("cannot tell is a refusal");
        assert!(err.to_string().contains("cannot be established"), "{err}");
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
}

#[cfg(all(test, unix))]
mod discovery_order_tests {
    //! THE STALE RUSTUP LINK, for the cutter (measured 2026-09-24 on the owner's Mac:
    //! `~/.rustup/toolchains/trust` -> `$HOME/trust/build/host/stage2`, 2026-08-20, the store
    //! at 9192, 2026-09-17). Driven through [`discovery_order`] with the real
    //! [`atpkg::seam::stale_against_store`] against a temp layout — nothing here reads
    //! this machine's rustup, store or PATH.

    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A sysroot at `dir` whose `bin/trustc` answers `-vV` with `commit-date: <date>`,
    /// beside an executable `targo`. Every `trustc` is a HARD LINK to one script run once
    /// here, unbounded: macOS assesses a new executable on its first exec (measured ~20 s
    /// on a loaded m7, 2026-09-24 — past atpkg's 5 s probe bound), and a link to an
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
mod provenance_gate_tests {
    use super::*;
    use crate::publish::CutKind;

    /// A fresh real cut started from a shell, not under the launcher.
    fn shell_cut() -> Rerun {
        Rerun {
            command: "tools/cut-launch.sh --release-credentials <path>".to_string(),
            under_launcher: false,
        }
    }

    /// Clean toolchain, untracked cutter: silent pass.
    #[test]
    fn a_clean_toolchain_under_an_untracked_cutter_passes() {
        assert!(provenance_verdict(&[], false, None, &shell_cut()).is_ok());
    }

    /// The v0.83.0 shape: a tagged trustc. The refusal names the path, the attribute,
    /// the post-claim failure it pre-empts, and the remedies — the heal (`aterm pkg
    /// repair`, the cutter's own), never a re-install that writes the same tagged files.
    #[test]
    fn a_tagged_trustc_is_refused_before_the_claim_with_the_remedies() {
        let trustc = PathBuf::from(
            "/Users//me/Library/Application Support/aterm/pkg/store/trust/8590/bin/trustc",
        );
        let err = provenance_verdict(&[("trustc", trustc.clone())], false, None, &shell_cut())
            .expect_err("a tagged compiler must not cut");
        let msg = err.to_string();
        assert!(msg.contains(&trustc.display().to_string()), "{msg}");
        assert!(msg.contains("trustc:"), "{msg}");
        assert!(msg.contains("BEFORE the claim"), "{msg}");
        assert!(msg.contains("proof_snapshot.py"), "what it breaks: {msg}");
        assert!(
            msg.contains("xattr -d"),
            "why nothing here can fix it: {msg}"
        );
        assert!(msg.contains("fix:  `aterm pkg repair`"), "remedy 1: {msg}");
        assert!(
            msg.contains("the cutter clears its own toolchain and binary"),
            "remedy 1, the cutter's half: {msg}"
        );
        assert!(
            !msg.contains("uninstall")
                && !msg.contains("re-seed")
                && !msg.contains("TRUST_STAGE2_BIN"),
            "no re-install, no lane, no environment knob: {msg}"
        );
        assert!(msg.contains("tools/cut-launch.sh"), "remedy 3: {msg}");
        // …and remedy 3 must not send the operator to the launcher that starves
        // the paint smoke. `launchctl submit` may appear only as the warning it
        // now is.
        assert!(
            msg.contains("not `launchctl submit`"),
            "remedy 3 names the wrong launcher without warning against it: {msg}"
        );
        assert!(msg.contains("QOS_CLASS_UTILITY"), "remedy 3: {msg}");
        assert!(
            !msg.contains("cutter PROCESS is provenance-tracked"),
            "not measured tracked: {msg}"
        );
    }

    /// A clean toolchain does not save a tracked cutter process: the tag follows the
    /// parent too, and the probe write is what shows it. Both causes are named — a cutter
    /// whose own binary the heal just cleared is tracked until it is started again, and the
    /// refusal must not blame a parent that may be clean.
    #[test]
    fn a_tracked_cutter_process_is_refused_even_with_a_clean_toolchain() {
        let err = provenance_verdict(&[], true, None, &shell_cut())
            .expect_err("a tracked cutter must not cut");
        let msg = err.to_string();
        assert!(
            msg.contains("cutter PROCESS is provenance-tracked"),
            "{msg}"
        );
        assert!(msg.contains("probe file"), "{msg}");
        assert!(
            msg.contains("its own binary carried the tag when it was started")
                && msg.contains("a fresh start can run clean"),
            "the cleared-binary cause: {msg}"
        );
        assert!(msg.contains("or it runs under a tracked parent"), "{msg}");
        assert!(
            msg.contains("\nfix:  tools/cut-launch.sh --release-credentials <path>"),
            "the launcher is the one fix for a tracked process: {msg}"
        );
        assert!(
            !msg.contains("aterm pkg repair"),
            "a repair clears files, never a running process: {msg}"
        );
    }

    /// Under the launcher the launcher is no remedy — the job already is it. What tracks
    /// that job, with the toolchain clean, is the targo shim it execs, which this gate
    /// neither reads nor clears and `aterm pkg repair` does; the refusal names the repair
    /// and then the same launcher run again.
    #[test]
    fn a_tracked_cut_under_the_launcher_is_sent_to_the_repair_not_back_to_the_launcher() {
        let under = Rerun {
            under_launcher: true,
            ..shell_cut()
        };
        let msg = provenance_verdict(&[], true, None, &under)
            .unwrap_err()
            .to_string();
        assert!(
            msg.ends_with(
                "\nfix:  aterm pkg repair   (it clears a tagged targo shim, which the job execs), \
                 then tools/cut-launch.sh --release-credentials <path>"
            ),
            "{msg}"
        );
        assert!(!msg.contains("untracked)"), "{msg}");
        // With a carrier left, the repair is the fix and the launcher is not offered again.
        let msg = provenance_verdict(
            &[("trustc", PathBuf::from("/s/bin/trustc"))],
            false,
            None,
            &under,
        )
        .unwrap_err()
        .to_string();
        assert!(msg.contains("fix:  `aterm pkg repair`"), "{msg}");
        assert!(!msg.contains("\nor:"), "{msg}");
    }

    /// A resume's and a dry run's refusal repeat THAT run: a resume that still has to
    /// rebuild is told `--resume` (a fresh cut is refused while one is journaled), and a
    /// dry run's copied fix must not start a real cut.
    #[test]
    fn the_launcher_fix_repeats_this_run() {
        for (rerun, spelled) in [
            (
                Rerun::resume(),
                "\nfix:  tools/cut-launch.sh --resume   (runs the cut",
            ),
            (
                Rerun::of(CutKind::DryRun, None),
                "\nfix:  tools/cut-launch.sh --dry-run --release-credentials <path>   (runs",
            ),
        ] {
            let rerun = Rerun {
                under_launcher: false,
                ..rerun
            };
            let msg = provenance_verdict(&[], true, None, &rerun)
                .unwrap_err()
                .to_string();
            assert!(msg.contains(spelled), "{rerun:?}: {msg}");
        }
    }

    /// Every carrier is named, in the order checked — the operator should not fix one
    /// and discover the next.
    #[test]
    fn every_carrier_is_named_at_once() {
        let err = provenance_verdict(
            &[
                ("trustc", PathBuf::from("/s/bin/trustc")),
                ("targo", PathBuf::from("/s/bin/targo")),
                ("the cutter's own binary", PathBuf::from("/t/cargo-ship")),
            ],
            true,
            None,
            &shell_cut(),
        )
        .unwrap_err();
        let msg = err.to_string();
        let a = msg.find("trustc: /s/bin/trustc").expect("trustc named");
        let b = msg.find("targo: /s/bin/targo").expect("targo named");
        let c = msg
            .find("the cutter's own binary: /t/cargo-ship")
            .expect("cutter named");
        assert!(a < b && b < c, "{msg}");
    }

    /// A tag the heal could not clear is still refused — the refusal stays for every
    /// carrier left — and the refusal says the heal ran and why it did not take.
    #[test]
    fn a_tag_the_heal_left_is_refused_and_says_the_heal_ran() {
        let trustc = PathBuf::from("/s/bin/trustc");
        let err = provenance_verdict(
            &[("trustc", trustc.clone())],
            false,
            Some("xattr exited 1"),
            &shell_cut(),
        )
        .expect_err("a tag that survived the heal must not cut");
        let msg = err.to_string();
        assert!(msg.contains("trustc: /s/bin/trustc carries"), "{msg}");
        assert!(
            msg.contains("launchd job first, and the tag stayed: xattr exited 1"),
            "{msg}"
        );
        assert!(msg.contains("the toolchain and the cutter"), "{msg}");
        assert!(msg.contains("BEFORE the claim"), "{msg}");
        // With nothing left tagged the heal's reason is not a carrier: a tracked cutter
        // is refused for itself alone.
        let msg = provenance_verdict(&[], true, Some("xattr exited 1"), &shell_cut())
            .unwrap_err()
            .to_string();
        assert!(!msg.contains("tag stayed"), "{msg}");
    }

    /// A store-shaped toolchain: `store/trust/current -> 9192`, the gate handed the
    /// `current/bin` spelling. The heal covers the REAL `bin/` and `lib/` — never the link
    /// (the heal does not follow one) — a `targo` in the same `bin/` adds nothing, one in
    /// another directory adds that directory, and the cutter's own binary is a root of its
    /// own as a FILE, never the `target/` it sits in.
    #[cfg(unix)]
    #[test]
    fn the_toolchain_heal_covers_the_real_bin_and_lib_and_the_cutter() {
        let d = env::temp_dir().join(format!("aterm-release-heal-roots-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let build = d.join("store/trust/9192");
        fs::create_dir_all(build.join("bin")).unwrap();
        fs::create_dir_all(build.join("lib")).unwrap();
        fs::write(build.join("bin/trustc"), b"x").unwrap();
        fs::write(build.join("bin/targo"), b"x").unwrap();
        std::os::unix::fs::symlink(&build, d.join("store/trust/current")).unwrap();
        let cutter = d.join("target/release/aterm-release");
        fs::create_dir_all(cutter.parent().unwrap()).unwrap();
        fs::write(&cutter, b"x").unwrap();
        let stage2 = d.join("store/trust/current/bin");
        let real = fs::canonicalize(&build).unwrap();
        let cutter_real = fs::canonicalize(&cutter).unwrap();
        let mut want = vec![real.join("bin"), real.join("lib"), cutter_real.clone()];
        want.sort();
        assert_eq!(
            toolchain_heal_roots(&stage2.join("trustc"), &stage2, &cutter),
            want
        );
        let elsewhere = d.join("targo-bin");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("targo"), b"x").unwrap();
        let mut want = vec![
            real.join("bin"),
            real.join("lib"),
            fs::canonicalize(&elsewhere).unwrap(),
            cutter_real.clone(),
        ];
        want.sort();
        assert_eq!(
            toolchain_heal_roots(&stage2.join("trustc"), &elsewhere, &cutter),
            want
        );
        // A bundle with no lib/ heals its bin/ alone; nothing resolvable heals nothing.
        fs::remove_dir_all(build.join("lib")).unwrap();
        let mut want = vec![real.join("bin"), cutter_real];
        want.sort();
        assert_eq!(
            toolchain_heal_roots(&stage2.join("trustc"), &stage2, &cutter),
            want
        );
        assert!(
            toolchain_heal_roots(
                &d.join("absent/trustc"),
                &d.join("absent"),
                &d.join("absent/cutter")
            )
            .is_empty()
        );
        let _ = fs::remove_dir_all(&d);
    }

    /// A scratch toolchain the way a cut sees it: `bin/trustc`, `bin/targo`, a dylib under
    /// `lib/`, and a cutter binary in a `target/` of its own.
    #[cfg(target_os = "macos")]
    struct ScratchToolchain {
        dir: PathBuf,
        trustc: PathBuf,
        stage2: PathBuf,
        cutter: PathBuf,
        files: [PathBuf; 4],
    }

    #[cfg(target_os = "macos")]
    impl ScratchToolchain {
        fn new(label: &str) -> Self {
            let dir =
                env::temp_dir().join(format!("aterm-release-heal-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            for sub in ["bundle/bin", "bundle/lib", "target/release"] {
                fs::create_dir_all(dir.join(sub)).unwrap();
            }
            let stage2 = dir.join("bundle/bin");
            let files = [
                stage2.join("trustc"),
                stage2.join("targo"),
                dir.join("bundle/lib/libstd-x.dylib"),
                dir.join("target/release/aterm-release"),
            ];
            for file in &files {
                fs::write(file, b"not a compiler").unwrap();
            }
            Self {
                trustc: files[0].clone(),
                cutter: files[3].clone(),
                stage2,
                files,
                dir,
            }
        }

        /// The gate's toolchain half — carriers of `attr` after `heal`, then the verdict
        /// for an untracked cutter process (the process probe is the one half a test
        /// process cannot choose, and it is pinned by the verdict tests above).
        fn gate(&self, heal: atpkg::provenance::Healer, attr: &str) -> Result<()> {
            let healed = heal_toolchain(&self.trustc, &self.stage2, &self.cutter, heal);
            let carriers = toolchain_carriers(&self.trustc, &self.stage2, &self.cutter, attr)?;
            provenance_verdict(&carriers, false, healed.why(), &shell_cut())
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for ScratchToolchain {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// The attribute the synthetic half tags with: one a test can set, read through the
    /// very `listxattr` path the gate reads.
    #[cfg(target_os = "macos")]
    const PROBE: &str = "user.aterm.probe";

    /// The real launchd heal job, over [`PROBE`].
    #[cfg(target_os = "macos")]
    fn heal_probe(roots: &[PathBuf], scratch: &Path) -> atpkg::provenance::HealOutcome {
        atpkg::provenance::heal_with(roots, scratch, PROBE)
    }

    /// A heal that clears nothing — the negative control.
    #[cfg(target_os = "macos")]
    fn heal_nothing(roots: &[PathBuf], _scratch: &Path) -> atpkg::provenance::HealOutcome {
        atpkg::provenance::HealOutcome::Unavailable {
            carriers: roots.to_vec(),
            why: String::from("the control heals nothing"),
        }
    }

    /// THE CUTTER CURES ITS OWN TOOLCHAIN, non-vacuously on any machine: every file of a
    /// scratch toolchain and the cutter's own binary carry a synthetic attribute; with a
    /// heal that clears nothing the gate refuses and names all four (and the dylib under
    /// its library label), and after the REAL launchd heal job over that attribute the gate
    /// passes with nothing tagged.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_tagged_scratch_toolchain_and_cutter_are_cured_and_then_pass_the_gate() {
        let t = ScratchToolchain::new("synthetic");
        for file in &t.files {
            let out = Command::new("/usr/bin/xattr")
                .args(["-w", PROBE, "1"])
                .arg(file)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let msg = t
            .gate(heal_nothing, PROBE)
            .expect_err("the negative control: nothing cleared, the gate refuses")
            .to_string();
        for (label, file) in [
            ("trustc", &t.files[0]),
            ("targo", &t.files[1]),
            ("a library trustc loads", &t.files[2]),
            ("the cutter's own binary", &t.files[3]),
        ] {
            assert!(
                msg.contains(&format!("{label}: {} carries", file.display())),
                "{label}: {msg}"
            );
        }
        assert!(
            msg.contains("the tag stayed: the control heals nothing"),
            "{msg}"
        );
        t.gate(heal_probe, PROBE)
            .expect("the heal clears the toolchain and the cutter, and the gate passes");
        for file in &t.files {
            assert!(
                !atpkg::provenance::carries(file, PROBE),
                "{}",
                file.display()
            );
        }
    }

    /// THE REAL TAG. Files this test process writes carry `com.apple.provenance` exactly
    /// when it is tracked (an agent's shell, a shell inside a tracked aterm.app) — the shape
    /// a tagged trust bundle and a cutter built from such a shell have. Tracked: the
    /// control refuses on the real attribute, and the real heal clears it so the gate
    /// passes. Untracked: nothing is tagged, both pass — vacuous, and said so on stderr.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_real_tag_on_a_scratch_toolchain_is_cured_before_the_gate_reads_it() {
        let attr = atpkg::provenance::PROVENANCE_XATTR;
        let t = ScratchToolchain::new("real");
        let tracked = atpkg::provenance::carries(&t.trustc, attr);
        eprintln!("this test process tracked={tracked}");
        let control = t.gate(heal_nothing, attr);
        assert_eq!(control.is_err(), tracked, "{control:?}");
        t.gate(atpkg::provenance::heal, attr)
            .expect("the real heal leaves nothing tagged");
        for file in &t.files {
            assert!(
                !atpkg::provenance::carries(file, attr),
                "{} still tagged",
                file.display()
            );
        }
    }

    /// The predicate the gate reads, on a real file with a synthetic attribute: listed
    /// where set, absent where not, and a missing path is an error (which the gate turns
    /// into a refusal, never a pass).
    #[cfg(target_os = "macos")]
    #[test]
    fn the_gate_reads_the_attribute_through_listxattr() {
        let d = env::temp_dir().join(format!("aterm-release-provenance-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let tagged = d.join("trustc");
        let clean = d.join("targo");
        fs::write(&tagged, b"x").unwrap();
        fs::write(&clean, b"x").unwrap();
        let out = Command::new("/usr/bin/xattr")
            .args(["-w", "user.aterm.probe", "1"])
            .arg(&tagged)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            atpkg::provenance::xattr_names(&tagged)
                .unwrap()
                .iter()
                .any(|n| n == "user.aterm.probe")
        );
        assert!(!atpkg::provenance::carries(&clean, "user.aterm.probe"));
        assert!(atpkg::provenance::xattr_names(&d.join("absent")).is_err());
        let _ = fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod native_lane_flag_tests {
    //! THE DRIFT THIS PINS IS INVISIBLE FROM THE MACHINE IT BREAKS.
    //!
    //! The macOS release gate reads one table name out of `.cargo/config.toml`
    //! to learn the flags its trustc probe must compile under. When ecb1d6691
    //! renamed that table on 2026-08-30 — two per-triple copies collapsed into
    //! one `[target.'cfg(trust_verify)']` — nothing went red: the lookup missed,
    //! the reader answered `[]`, and every probe since compiled under no flags
    //! and could not fail on them. The gate existed precisely to catch a flag
    //! drift before a cut, and for a fortnight it caught nothing.
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
    /// on one.
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
            .filter(|(_, flags)| flags.iter().any(|flag| flag.contains("trust-verify")))
            .collect();
        assert!(
            !trust_lane.is_empty(),
            "no [target.…] table in {} carries a trust-verify flag. If the Trust-Std \
             campaign really greened, that is a deliberate edit in gates.rs \
             (native_lane_rustflags) and in this test — not a silent one. Tables with \
             rustflags: {carriers:?}",
            path.display()
        );
        for (name, _) in &trust_lane {
            assert!(
                TRUST_LANE_TABLES.contains(&name.as_str()),
                "[target.{name:?}] carries the Trust lane's verification off-switch and \
                 gates.rs does not know that name: TRUST_LANE_TABLES is \
                 {TRUST_LANE_TABLES:?}. The reader would answer [] and the release's \
                 trustc probe would compile under no flags at all — the 2026-08-30 \
                 regression, exactly. Add the name to TRUST_LANE_TABLES."
            );
        }

        let read = native_lane_rustflags(&repo).expect("the shipped config reads");
        assert!(
            trust_lane.iter().any(|(_, flags)| *flags == read),
            "native_lane_rustflags answered {read:?}, which is no table's rustflags in \
             {}: {trust_lane:?}",
            path.display()
        );
        assert!(
            !read.is_empty() && read.iter().any(|flag| flag.contains("trust-verify")),
            "the flags the probe would compile under say nothing about verification: \
             {read:?}"
        );
    }

    /// The 2026-08-30 shape itself: the Trust table renamed away, the Windows
    /// cross-compile table left standing. The old reader called this the green
    /// campaign and returned `[]`.
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
        assert!(msg.contains("2026-08-30"), "why this refusal exists: {msg}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A table with no `rustflags` is not a Trust-lane table, whatever its name.
    #[test]
    fn a_table_without_rustflags_is_not_a_carrier() {
        let config: aterm_toml::Value =
            "[target.'cfg(trust_verify)']\nrustdocflags = [\"-Ztrust-verify=off\"]\n"
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
    /// the measured 2026-09-16 batteries-on output for [`PROBE_SRC`], and it is
    /// the reason the probe source carries an obligation — with `fn main() {}`
    /// there is no such line to read, in either lane.
    #[test]
    fn a_switch_that_parsed_but_did_not_switch_is_refused() {
        let canonical = vec!["-Ztrust-verify=off".to_string()];
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
            "PROBE_SRC lost the divisor-is-zero obligation the 2026-09-16 measurement \
             was made against; without an obligation verification_off_verdict is \
             unfalsifiable: {PROBE_SRC}"
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
    /// (`tools/release-preflight.sh`: `fix "pub bump aterm --minor --write"`): a hand
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
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let dir = env::temp_dir().join(format!(
                "aterm-release-handoff-{label}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("scratch repo");
            Scratch(dir)
        }
        fn write(&self, rel: &str, text: &str) {
            let path = self.0.join(rel);
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

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
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
        handoff_fixture_gate(origin, &scratch.0, version)
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
            msg.contains(&format!("worktree at tag v{release}")),
            "{msg}"
        );
        assert!(msg.contains("before the claim"), "{msg}");
    }

    /// The case this gate exists for: v0.92.0 shipped and its directory was
    /// never added. Before 2026-09-24 the cut went ahead.
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
        let found = handoff_fixture_gate(&origin, &scratch.0, "0.93.0")
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
        let found = handoff_fixture_gate(&origin, &scratch.0, "0.93.0")
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
        let found = handoff_fixture_gate(&origin, &scratch.0, "0.94.0")
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

    /// THE DESK SET (the round-4 update audit, plan item 5). From the guard's
    /// floor on, one pinned desk is not enough: a predecessor missing a
    /// required desk — not on disk, or on disk and not pinned — refuses the
    /// cut, naming what is missing. RED before item 5: `handoff_fixtures_of`
    /// asked only for one pinned row, and this predecessor has three.
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
        let found = handoff_fixture_gate(&origin, &scratch.0, "0.98.0")
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
        let found = handoff_fixture_gate(&origin, &scratch.0, "0.97.0")
            .expect("v0.95.0 predates the floor")
            .expect("a predecessor");
        assert_eq!(found.release, "0.95.0");
        // Ordered as numbers, not strings: 0.100.0 is past a 0.97.0 floor.
        assert!(release_at_least("0.100.0", "0.97.0").expect("orders"));
        assert!(!release_at_least("0.9.0", "0.97.0").expect("orders"));
        assert!(release_at_least("0.97", "0.97.0").is_err(), "not X.Y.Z");
    }

    /// A DESK ADDED FOR A LATER RELEASE ASKS NOTHING OF AN EARLIER ONE (the
    /// round-four review). The guard gains the next release's `split-sequences`
    /// desk, required from v0.98.0 on: cutting after v0.97.0 — whose frozen
    /// fixtures can never gain it — still passes on v0.97.0's own desks, and
    /// cutting after v0.98.0 asks for it.
    ///
    /// FAILS WITHOUT THE FIX: the table had one floor for the whole set, so the
    /// only way to require the new desk was to require it of v0.97.0 too, and
    /// this cut was refused (read with the old parser, the pairs below were a
    /// flat list whose `v0.97.0` "desk" was missing).
    #[test]
    fn a_desk_added_for_a_later_release_asks_nothing_of_an_earlier_one() {
        let scratch = Scratch::new("required-per-desk");
        scratch.ledger(&["0.97.0", "0.98.0"]);
        scratch.desk("0.97.0", "history");
        scratch.desk("0.98.0", "history");
        let required = [("v0.97.0", "history"), ("v0.98.0", "split-sequences")];
        scratch.pins_required_rows(&[("v0.97.0", "history"), ("v0.98.0", "history")], &required);
        let origin = Origin::with(&["v0.97.0"]);
        let found = handoff_fixture_gate(&origin, &scratch.0, "0.98.0")
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
        let found = handoff_fixture_gate(&origin, &scratch.0, "0.99.0")
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
            &scratch.0,
            &mut |command| {
                let target = scratch.0.join(HANDOFF_FIXTURE_GUARD_TARGET);
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

    /// THE GUARD RUNS IN THE CUT TREE, before the claim (plan item 5): a red
    /// guard refuses the cut, naming the failing test, and the guard's target
    /// directory is gone afterwards. RED before item 5: nothing in the cut ran
    /// the guard, so a consumer that refused a shipped producer's desk shipped.
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
        assert!(msg.contains("before the claim"), "{msg}");
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
                    .join(HANDOFF_FIXTURE_GUARD_TARGET)
                    .into_os_string(),
                "--".into(),
                HANDOFF_FIXTURE_GUARD_FILTER.into(),
            ]
        );
        assert_eq!(
            run.dir.as_deref(),
            Some(scratch.0.as_path()),
            "in the cut tree"
        );
        for steering in ["RUSTFLAGS", "RUSTC", "RUSTUP_TOOLCHAIN", "CARGO_TARGET_DIR"] {
            assert!(
                run.removed.iter().any(|key| key == steering),
                "{steering} is scrubbed"
            );
        }
        assert!(
            !scratch.0.join(HANDOFF_FIXTURE_GUARD_TARGET).exists(),
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
        assert!(!scratch.0.join(HANDOFF_FIXTURE_GUARD_TARGET).exists());
    }

    #[test]
    fn tag_listing_folds_peeled_rows_and_ignores_other_refs() {
        let listing = "aaa\trefs/tags/v0.92.0\nbbb\trefs/tags/v0.92.0^{}\n\
                       ccc\trefs/tags/public/v0.92.0\nddd\trefs/heads/main\n";
        let tags = tag_names(listing);
        assert!(tags.contains("v0.92.0"));
        assert!(!tags.contains("v0.92.0^{}"));
        assert!(tags.contains("public/v0.92.0"), "kept, but never `v…`");
        assert_eq!(tags.len(), 2, "{tags:?}");
    }

    /// THE REAL TREE. For every release the real ledger records that has a
    /// fixture directory, the cut after it passes against the real fixtures and
    /// the real guard source — so the gate reads this repository's actual
    /// layout and `PINNED_DESKS`, and v0.91.0 and v0.92.0 (the release the next
    /// cut succeeds, as of 2026-09-24) are both covered.
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
    use aterm_update_core::handoff_policy::{CarryCeiling, SOURCE_PATH};

    /// A scratch repository holding only `publish/handoff-policy.toml`, removed
    /// on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn with_policy(label: &str, text: Option<&str>) -> Self {
            let dir = env::temp_dir().join(format!(
                "aterm-release-policy-{label}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(dir.join("publish")).expect("scratch repo");
            if let Some(text) = text {
                fs::write(dir.join(SOURCE_PATH), text).expect("write the policy");
            }
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The file the next cut seals, as checked in, passes — so a policy edit
    /// that would refuse the cut fails here first, in the commit that made it.
    #[test]
    fn the_checked_in_policy_passes_the_gate() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/aterm-release sits two levels under the root");
        handoff_policy_gate(root).expect("the checked-in policy is one producers can follow");
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
        ] {
            let scratch = Scratch::with_policy(label, text);
            let refused = handoff_policy_gate(&scratch.0)
                .err()
                .unwrap_or_else(|| panic!("{label}: the gate passed"))
                .to_string();
            assert!(
                refused.contains("handoff-policy gate") && refused.contains(why),
                "{label}: {refused}"
            );
        }
        let scratch = Scratch::with_policy(
            "valid",
            Some(
                "schema = 1\napplies_to_producers = [1790120000, 1790129999]\ncarry = \"repaint\"\n",
            ),
        );
        let policy = handoff_policy_gate(&scratch.0).expect("a v1 policy passes");
        assert_eq!(policy.carry, Some(CarryCeiling::Repaint));
        assert!(policy.applies_to(1_790_120_001) && !policy.applies_to(1_790_130_000));
    }
}

#[cfg(test)]
mod staged_bundle_liveness_tests {
    //! THE PRE-CLAIM LIVENESS GATE (2026-09-23): a live process under a cut staging
    //! bundle refuses, "could not look" refuses, and only an answered empty look
    //! passes. The owner's aterm ran out of `dist/cut-app/aterm.app` (pid 85619) while
    //! `bundle::assemble` would have `rm -rf`'d that directory unchecked.

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
        // like one, an install elsewhere, and the fixed `dist/cut-app/` the cut
        // assembled in until 2026-09-23 (pid 85619 ran there): no cut deletes it any
        // more, so nothing running out of it is at risk or in the way.
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
    //! THE CUT BUILDS THE PUBLISHED COMMIT (2026-09-23), against real git: the cut
    //! tree goes to the commit `pub publish` recorded whatever main's tip is, the
    //! operator's checkout never moves, and the transcript states how far that commit is past the newest gate
    //! receipt, by [`receipt_report`]'s predicate.

    use super::*;
    use crate::ledger::GitCli;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    struct Repo {
        root: PathBuf,
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    impl Repo {
        fn new(label: &str) -> Self {
            let root = env::temp_dir().join(format!(
                "aterm-release-published-{label}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("work")).unwrap();
            let repo = Self { root };
            repo.run(&["init", "-q", "-b", "main"]);
            repo.run(&["config", "user.name", "Test"]);
            repo.run(&["config", "user.email", "test@example.invalid"]);
            repo.run(&["config", "commit.gpgsign", "false"]);
            repo
        }
        fn work(&self) -> PathBuf {
            self.root.join("work")
        }
        fn run(&self, args: &[&str]) -> String {
            let out = Command::new("git")
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
        fn commit(&self, subject: &str, files: &[(&str, &str)]) -> String {
            for (path, body) in files {
                let full = self.work().join(path);
                fs::create_dir_all(full.parent().unwrap()).unwrap();
                fs::write(full, body).unwrap();
                self.run(&["add", path]);
            }
            self.run(&["commit", "-q", "--allow-empty", "-m", subject]);
            self.run(&["rev-parse", "HEAD"])
        }
        /// A bare `origin` holding main as it stands now.
        fn publish_origin(&self) {
            let origin = self.root.join("origin.git");
            let out = Command::new("git")
                .args(["clone", "-q", "--bare"])
                .arg(self.work())
                .arg(&origin)
                .output()
                .unwrap();
            assert!(out.status.success());
            self.run(&["remote", "add", "origin", origin.to_str().unwrap()]);
            self.run(&["fetch", "-q", "origin"]);
        }
        fn push(&self) {
            self.run(&["push", "-q", "origin", "main"]);
        }
        fn git(&self) -> GitCli {
            GitCli::new(self.work())
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
        /// A whole-tree PASS filed the way the gate files one since 2026-09-26:
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
        let tree = repo.root.join("work-cut.noindex");
        assert_eq!(checkout.tree, tree, "the cut tree sits beside the checkout");
        assert!(checkout.moved);
        assert_eq!(checkout.main_ahead, 2);
        let in_tree = |args: &[&str]| {
            let out = Command::new("git")
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
            !repo.root.join("work-cut.noindex").exists(),
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
    }

    /// A directory at the cut tree's path that is not this repository's worktree is
    /// never touched — the cutter refuses and names it.
    #[test]
    fn a_foreign_directory_where_the_cut_tree_goes_is_refused_and_left_alone() {
        let repo = Repo::new("foreign");
        let published = repo.commit("publish me", &[("a", "1")]);
        repo.publish_origin();
        let foreign = repo.root.join("work-cut.noindex");
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
        let report = receipt_report(&repo.git(), &repo.receipts()).unwrap();
        assert_eq!(
            report.newest_gated,
            Some((base[..9].to_string(), "base".to_string()))
        );
        assert_eq!(report.ungated, vec![format!("{} head", &head[..9])]);
        // The control: a whole-tree receipt on HEAD is a gate.
        repo.receipt(&head, "PASS", "yes");
        let report = receipt_report(&repo.git(), &repo.receipts()).unwrap();
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

    /// A COMMIT GATED WITH INHERITED REDS SAYS WHICH (2026-09-26). The gate's
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
        let report = receipt_report(&repo.git(), &repo.receipts()).unwrap();
        assert_eq!(report.ungated.len(), 0);
        assert_eq!(report.inherited, ["tippy lint", "-p x --lib -- flaky"]);

        // By its tree: a reworded commit over the same bytes carries them too.
        fs::remove_file(repo.receipts().join(&gated)).unwrap();
        fs::write(
            repo.receipts().join(format!("{RECEIPT_TREE_PREFIX}{tree}")),
            &text,
        )
        .unwrap();
        let report = receipt_report(&repo.git(), &repo.receipts()).unwrap();
        assert!(report.gated_by_tree.is_some());
        assert_eq!(report.inherited.len(), 2);

        // The control: a commit gated clean names nothing.
        let clean = repo.commit("gated clean", &[("a", "2")]);
        repo.receipt(&clean, "PASS", "yes");
        let report = receipt_report(&repo.git(), &repo.receipts()).unwrap();
        assert!(report.inherited.is_empty(), "{report:?}");
    }

    /// A TREE PASS WITH INHERITED REDS GATES ONLY ITS OWN BASE (2026-09-27). It
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
        let report = receipt_report(&repo.git(), &repo.receipts()).unwrap();
        assert!(report.ungated.is_empty(), "the same parents: {report:?}");
        assert_eq!(report.gated_by_tree.as_deref(), Some(&gated[..9]));

        repo.commit("elsewhere", &[("a", "3")]);
        let moved = repo.commit("the same bytes on another parent", &[("a", "2")]);
        assert_eq!(repo.run(&["rev-parse", &format!("{moved}^{{tree}}")]), tree);
        let report = receipt_report(&repo.git(), &repo.receipts()).unwrap();
        assert_eq!(report.ungated.len(), 2, "{report:?}");
        assert_eq!(
            report.newest_gated.map(|(sha, _)| sha),
            Some(amended[..9].to_string())
        );

        fs::write(&file, text("")).unwrap();
        let report = receipt_report(&repo.git(), &repo.receipts()).unwrap();
        assert!(report.ungated.is_empty(), "a clean pass: {report:?}");
        assert_eq!(report.gated_by_tree.as_deref(), Some(&gated[..9]));
    }

    /// A NEAREST-BASE RECEIPT IS READ EXACTLY AS ANY OTHER (2026-09-28). A
    /// run judged against an ancestor of its merge-base (the gate's opt-in
    /// `--nearest-base`) writes `base-mode nearest <commit>`; the report over
    /// it — by the commit, by the tree for a reworded amend, and NOT for the
    /// same bytes on another parent — is what the same receipt without that
    /// line gives, so no cut requirement is relaxed (or tightened) by it.
    #[test]
    fn a_nearest_base_receipt_gates_exactly_as_an_exact_one() {
        let outcomes = |nearest: bool| {
            let repo = Repo::new(if nearest {
                "receipts-nearest"
            } else {
                "receipts-exact"
            });
            let base = repo.commit("base", &[("a", "1")]);
            let gated = repo.commit("gated with main's reds", &[("a", "2")]);
            let tree = repo.run(&["rev-parse", &format!("{gated}^{{tree}}")]);
            let mode = if nearest {
                format!("base-mode nearest {base}\n")
            } else {
                String::new()
            };
            let text = format!(
                "{RECEIPT_MAGIC}\nhead {gated}\ntree {tree}\nmode fast\nscope workspace\n\
                 verdict PASS\nmerge-contract yes\nskipped none\nfailures 1\n\
                 fail 00000000000000aa {base} 1 -p x --lib -- flaky\nbase {base}\n{mode}\
                 inherited -p x --lib -- flaky\nwhen 1\n"
            );
            let by_commit = repo.receipts().join(&gated);
            fs::write(&by_commit, &text).unwrap();
            let own = receipt_report(&repo.git(), &repo.receipts()).unwrap();
            fs::remove_file(&by_commit).unwrap();
            fs::write(
                repo.receipts().join(format!("{RECEIPT_TREE_PREFIX}{tree}")),
                &text,
            )
            .unwrap();
            repo.run(&["commit", "-q", "--amend", "-m", "gated, reworded"]);
            let amended = receipt_report(&repo.git(), &repo.receipts()).unwrap();
            repo.commit("elsewhere", &[("a", "3")]);
            repo.commit("the same bytes on another parent", &[("a", "2")]);
            let moved = receipt_report(&repo.git(), &repo.receipts()).unwrap();
            (
                (
                    own.ungated.len(),
                    own.inherited,
                    own.gated_by_tree.is_some(),
                ),
                (
                    amended.ungated.len(),
                    amended.inherited,
                    amended.gated_by_tree.is_some(),
                ),
                (moved.ungated.len(), moved.inherited.len()),
            )
        };
        let exact = outcomes(false);
        assert_eq!(
            exact,
            (
                (0, vec!["-p x --lib -- flaky".to_string()], false),
                (0, vec!["-p x --lib -- flaky".to_string()], true),
                (2, 1),
            ),
            "the exact-base receipt, as the cutter reads it today"
        );
        assert_eq!(outcomes(true), exact);
    }

    /// The compiler the measure tests' cut builds with.
    const CUT_TC: &str = "/store/trust/9192/bin trustc 43f8b339f8322f6b4cd1d9f7ae9a914eda5fef22";

    /// NOTHING SHIPS UNMEASURED (2026-09-26), against real git and the store
    /// the gate writes. With nothing measured, a real cut REFUSES — naming the
    /// command that measures the tree — and a dry run states that it would;
    /// a merge-contract PASS is no measurement; a MEASURE receipt that says
    /// `measured no`, one about another commit, and a narrowed one that says
    /// `measured yes` are refused, each named; `measured yes` under the tree's
    /// MEASURE key, from another commit's run over the same bytes, passes and
    /// names that run; and `measured yes` under the commit's own key passes.
    #[test]
    fn a_real_cut_refuses_a_tree_no_measure_receipt_measured() {
        let repo = Repo::new("measure");
        let head = repo.commit("ship me", &[("src/lib.rs", "v1")]);
        let tree = repo.run(&["rev-parse", "HEAD^{tree}"]);
        let git = repo.git();
        let measure = |key: &str, run: &str, scope: &str, measured: &str| {
            fs::write(
                repo.receipts().join(key),
                format!(
                    "{RECEIPT_MAGIC}\nhead {run}\ntree {tree}\nmode measure\nscope {scope}\n\
                     verdict PASS\nmerge-contract no\nmeasured {measured}\nskipped none\n\
                     toolchain {CUT_TC}\nbuild-env none\nbuild-config none\nwhen 1\n"
                ),
            )
            .unwrap();
        };
        let measure_gate =
            |git: &dyn GitRunner, refuse| measure_gate_with(git, refuse, CUT_TC, "HEAD");
        let measure_report = |git: &dyn GitRunner, receipts: &Path| {
            super::measure_report(git, receipts, CUT_TC, "HEAD")
        };
        let (by_commit, by_tree) = (
            format!("{RECEIPT_MEASURE_PREFIX}{head}"),
            format!("{RECEIPT_MEASURE_PREFIX}{RECEIPT_TREE_PREFIX}{tree}"),
        );
        let refused = |why: &str| {
            let err = measure_gate(&git, true)
                .expect_err("a real cut refuses")
                .to_string();
            assert!(err.contains("was never MEASURED"), "{err}");
            assert!(err.contains(why), "missing {why:?}: {err}");
            assert!(
                err.contains(&format!("git worktree add --detach <dir> {head}"))
                    && err.contains("tools/verify.sh --measure")
                    && err.contains("Nothing was claimed."),
                "{err}"
            );
            let line = measure_gate(&git, false).expect("a dry run states it");
            assert!(line.contains("NOT measured") && line.contains("a real cut would refuse"));
        };
        refused(&format!("no MEASURE receipt for {}", short(&head)));
        // A merge-contract PASS is not a measurement.
        repo.gate_receipt(&head);
        refused("no MEASURE receipt");
        measure(&by_commit, &head, "workspace", "no");
        refused(&format!(
            "`{RECEIPT_MEASURE_PREFIX}{}` says `measured no`",
            short(&head)
        ));
        measure(&by_commit, &"e".repeat(40), "workspace", "yes");
        refused("is about another head");
        measure(&by_commit, &head, "crate:aterm-conformance", "yes");
        refused("is a narrowed run's");
        // By the tree, from another commit's run over the same bytes.
        let other = "f".repeat(40);
        measure(&by_tree, &other, "workspace", "yes");
        assert_eq!(
            measure_report(&git, &repo.receipts()).unwrap(),
            MeasureReport::Measured {
                key: format!(
                    "{RECEIPT_MEASURE_PREFIX}{RECEIPT_TREE_PREFIX}{}",
                    short(&tree)
                ),
                run: short(&other).to_string(),
            }
        );
        let line = measure_gate(&git, true).expect("measured by its tree");
        assert!(
            line.contains("MEASURED") && line.contains(short(&other)),
            "{line}"
        );
        // …and by its own commit's key, which is read first.
        measure(&by_commit, &head, "workspace", "yes");
        assert_eq!(
            measure_report(&git, &repo.receipts()).unwrap(),
            MeasureReport::Measured {
                key: format!("{RECEIPT_MEASURE_PREFIX}{}", short(&head)),
                run: short(&head).to_string(),
            }
        );

        // THE SAME COMPILER (2026-09-27, second review): the attack's own
        // fixture — `measured yes` for these bytes with no `toolchain` line —
        // MEASURED the tree for any cut. Now a receipt that names no compiler,
        // or another one, is no measurement for this cut, under either key.
        let retool = |key: &str, run: &str, toolchain: Option<&str>| {
            let text = fs::read_to_string(repo.receipts().join(key)).unwrap();
            let kept: String = text
                .lines()
                .filter(|l| !l.starts_with("toolchain "))
                .map(|l| format!("{l}\n"))
                .collect();
            let text = match toolchain {
                Some(t) => kept.replace("when 1\n", &format!("toolchain {t}\nwhen 1\n")),
                None => kept,
            };
            assert!(text.contains(&format!("head {run}\n")), "{text}");
            fs::write(repo.receipts().join(key), text).unwrap();
        };
        let older = "/store/trust/9100/bin trustc 0000000000000000000000000000000000000000";
        retool(&by_commit, &head, None);
        retool(&by_tree, &other, Some(older));
        refused("names no compiler, and this cut builds with");
        refused(&format!("was measured with `{older}`"));
        // A trustc that names no commit cannot be told from another build.
        let unknown = "/store/trust/9192/bin trustc unknown";
        retool(&by_commit, &head, Some(unknown));
        let err = measure_gate_with(&git, true, unknown, "HEAD")
            .expect_err("no commit, no match")
            .to_string();
        assert!(err.contains("name no commit"), "{err}");
        // The control: the cut's own compiler under the tree's key measures.
        retool(&by_tree, &other, Some(CUT_TC));
        let line = measure_gate(&git, true).expect("the same compiler");
        assert!(line.contains(CUT_TC), "{line}");

        // THE SAME BUILD (2026-09-27, third review): a measurement of a release
        // build made under the caller's `RUSTFLAGS` — or by a run that does not
        // say — measured a binary this cut, whose builds clear their
        // environment, does not build.
        let reenv = |key: &str, env: Option<&str>| {
            let text = fs::read_to_string(repo.receipts().join(key)).unwrap();
            let kept: String = text
                .lines()
                .filter(|l| !l.starts_with("build-env "))
                .map(|l| format!("{l}\n"))
                .collect();
            let text = match env {
                Some(e) => kept.replace("when 1\n", &format!("build-env {e}\nwhen 1\n")),
                None => kept,
            };
            fs::write(repo.receipts().join(key), text).unwrap();
        };
        let flagged = "RUSTFLAGS=\"-Copt-level=0\"";
        reenv(&by_tree, Some(flagged));
        refused(&format!("measured a release build under `{flagged}`"));
        reenv(&by_tree, None);
        refused("does not say what build environment its release build took");
        reenv(&by_tree, Some("none"));
        let line = measure_gate(&git, true).expect("the same build");
        assert!(line.contains("MEASURED"), "{line}");

        // THE SAME CONFIG FILES (2026-09-28): a measurement whose builds read
        // a `~/.cargo/config.toml` or an ancestor's config — or a run that
        // does not say — measured a binary this cut, which reads the tree's
        // own config in an empty Cargo home, does not build.
        let reconfig = |key: &str, config: Option<&str>| {
            let text = fs::read_to_string(repo.receipts().join(key)).unwrap();
            let kept: String = text
                .lines()
                .filter(|l| !l.starts_with("build-config "))
                .map(|l| format!("{l}\n"))
                .collect();
            let text = match config {
                Some(c) => kept.replace("when 1\n", &format!("build-config {c}\nwhen 1\n")),
                None => kept,
            };
            fs::write(repo.receipts().join(key), text).unwrap();
        };
        let home = "cargo-home/config.toml=0123456789abcdef0123456789abcdef01234567";
        reconfig(&by_tree, Some(home));
        refused(&format!(
            "measured a release build reading the cargo config files `{home}`, and this cut \
             reads only the tree's own (`none`)"
        ));
        reconfig(&by_tree, None);
        refused("does not say what cargo config files its release build read");
        reconfig(&by_tree, Some("none"));
        let line = measure_gate(&git, true).expect("the same config files");
        assert!(line.contains("MEASURED"), "{line}");
    }

    /// The tree's own config, as the gate records it: git's blob id of each of
    /// `.cargo/config` and `.cargo/config.toml` the tree holds, in that order
    /// — never a directory of that name, and `none` for neither. A MEASURE
    /// receipt that read the tree's config passes; one that read an older
    /// version of it, or it and an ancestor's, does not.
    #[test]
    fn a_measurement_reads_the_trees_own_cargo_config_and_nothing_else() {
        let repo = Repo::new("measure-config");
        repo.commit("no config", &[("src/lib.rs", "v1")]);
        let git = repo.git();
        assert_eq!(tree_build_config(&git, "HEAD").unwrap(), "none");
        repo.commit(
            "configs",
            &[
                (".cargo/config.toml", "[build]\nrustflags = []\n"),
                (".cargo/config/nested", "a directory is no config file"),
            ],
        );
        let blob = repo.run(&["rev-parse", "HEAD:.cargo/config.toml"]);
        let own = format!("repo/.cargo/config.toml={blob}");
        assert_eq!(tree_build_config(&git, "HEAD").unwrap(), own);

        let head = repo.run(&["rev-parse", "HEAD"]);
        let tree = repo.run(&["rev-parse", "HEAD^{tree}"]);
        let key = format!("{RECEIPT_MEASURE_PREFIX}{head}");
        let write = |config: &str| {
            fs::write(
                repo.receipts().join(&key),
                format!(
                    "{RECEIPT_MAGIC}\nhead {head}\ntree {tree}\nmode measure\nscope workspace\n\
                     verdict PASS\nmerge-contract no\nmeasured yes\nskipped none\n\
                     toolchain {CUT_TC}\nbuild-env none\nbuild-config {config}\nwhen 1\n"
                ),
            )
            .unwrap();
        };
        write(&own);
        assert!(matches!(
            measure_report(&git, &repo.receipts(), CUT_TC, "HEAD").unwrap(),
            MeasureReport::Measured { .. }
        ));
        for other in [
            "none".to_string(),
            "repo/.cargo/config.toml=0000000000000000000000000000000000000000".to_string(),
            format!("{own} ancestor/.cargo/config.toml={blob}"),
            format!("{own} repo/.cargo/config.toml+include:x.toml=missing"),
        ] {
            write(&other);
            let MeasureReport::Unmeasured { why } =
                measure_report(&git, &repo.receipts(), CUT_TC, "HEAD").unwrap()
            else {
                panic!("measured reading `{other}`, and this cut reads `{own}`");
            };
            assert!(
                why.contains(&format!("reads only the tree's own (`{own}`)")),
                "{why}"
            );
        }
    }

    /// NOTHING SHIPS UNVERIFIED BY THE WHOLE-TREE TRUST LANE (2026-09-27), against
    /// real git and the store the lane writes. With no trust receipt a real cut
    /// REFUSES — naming the command that files one — and a dry run states that it
    /// would; a merge-contract PASS is not a trust run; a FAIL receipt, one about
    /// another tree, one without the magic line, one from another prover and one
    /// whose prover (or the cut's) names no commit are refused, each named; a PASS
    /// from the cut's own prover verifies, and says what it did not verify and
    /// what waits for a re-base.
    #[test]
    fn a_real_cut_refuses_a_tree_no_whole_tree_trust_run_passed() {
        const PROVER: &str = "321aaeda75478038420d97830f14724c33c8dc39";
        let repo = Repo::new("trust");
        let head = repo.commit("ship me", &[("src/lib.rs", "v1")]);
        let tree = repo.run(&["rev-parse", "HEAD^{tree}"]);
        let git = repo.git();
        let key = format!("{RECEIPT_TRUST_PREFIX}{tree}");
        let write = |about: &str, trustc: &str, verdict: &str, rebase: &str| {
            fs::write(
                repo.receipts().join(&key),
                format!(
                    "{TRUST_RECEIPT_MAGIC}\ntree {about}\nhead {head}\ntrustc {trustc}\n\
                     driver targo 1.99.0-dev (321aaeda7 2026-09-17) (targo 0.1.0)\n\
                     verdict {verdict}\ntotal 5/20\ngated 3\nnot-verified none\n\
                     rebase-pending {rebase}\ndate 2026-09-27T00:00:00Z\n"
                ),
            )
            .unwrap();
        };
        let gate = |refuse| trust_receipt_gate_with(&git, refuse, PROVER, "HEAD");
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
        refused(&format!("no trust receipt for its tree {}", short(&tree)));
        // A merge-contract PASS is not a whole-tree trust run.
        repo.gate_receipt(&head);
        refused("no trust receipt");
        write(&tree, PROVER, "FAIL", "none");
        refused("says `verdict FAIL`");
        // A run trustd refused decided nothing: no pass either.
        write(&tree, PROVER, "COULD-NOT-RUN", "none");
        refused("says `verdict COULD-NOT-RUN`");
        write(&"a".repeat(40), PROVER, "PASS", "none");
        refused("is not a trust receipt about this tree");
        fs::write(
            repo.receipts().join(&key),
            format!("tree {tree}\nverdict PASS\n"),
        )
        .unwrap();
        refused("is not a trust receipt about this tree");
        write(&tree, &"0".repeat(40), "PASS", "none");
        refused("was proved by trustc 000000000 and this cut builds with trustc 321aaeda7");
        write(&tree, "unknown", "PASS", "none");
        refused("cannot be told from another");
        // The control: a PASS from the cut's own prover verifies.
        write(&tree, PROVER, "PASS", "none");
        let line = gate(true).expect("verified");
        assert!(
            line.contains("VERIFIED whole-tree")
                && line.contains("5/20")
                && !line.contains("re-base"),
            "{line}"
        );
        // A cut prover that names no commit verifies nothing, whatever the receipt.
        let err = trust_receipt_gate_with(&git, true, UNKNOWN_COMMIT, "HEAD")
            .expect_err("no commit, no match")
            .to_string();
        assert!(err.contains("cannot be told from another"), "{err}");
        // Floors another prover recorded are stated, not refused.
        write(&tree, PROVER, "PASS", " aterm-grid aterm-core");
        let line = gate(true).expect("verified, with a re-base pending");
        assert!(
            line.contains("to re-base after the cut") && line.contains("aterm-grid"),
            "{line}"
        );
        // The prover is read out of the cut's own identity line.
        assert_eq!(
            trustc_commit_of(CUT_TC),
            "43f8b339f8322f6b4cd1d9f7ae9a914eda5fef22"
        );
        assert_eq!(trustc_commit_of("no identity"), UNKNOWN_COMMIT);
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

    /// THE SOURCE IS NEVER PUBLISHED AHEAD OF A BINARY THAT CANNOT FOLLOW IT
    /// (2026-09-29), against real git: `ship check` judges the COMMIT it is
    /// named — its objects and the receipts for it, never this checkout — with
    /// the cut's own gates in the cut's order, runs every gate after the
    /// cutter identity whatever the one before it said, and names each one
    /// that would refuse, in the cut's words. With the predecessor's fixtures,
    /// release notes and a handoff policy in the commit and a MEASURE and a
    /// TRUST receipt for it from the cut's compiler, it passes, however far
    /// HEAD and the working tree have moved.
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
        let tc = || Ok(CUT_TC.to_string());
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
                "ok      cutter identity: this check was built from {} itself",
                short(&candidate)
            ),
            "ok      CHANGELOG [Unreleased]: 1 entries, no '''",
            "ok      handoff fixtures of v0.1.0 checked in: 1 desks, 1 pinned",
            "ok      handoff policy a cut seals into the bundle: ",
            &format!(
                "REFUSE  MEASURE tier — the cut's own refusal: the tree this cut builds \
                 ({candidate}) was never MEASURED"
            ),
            &format!(
                "git worktree add --detach <dir> {candidate}`, then `tools/verify.sh --measure`"
            ),
            &format!(
                "REFUSE  TRUST lane — the cut's own refusal: the tree this cut builds \
                 ({candidate}) has no passing whole-tree Trust advisory run"
            ),
            &format!(
                "git worktree add --detach <dir> {candidate}`, then `tools/trust-gate-all.sh`"
            ),
        ] {
            assert!(text.contains(needle), "missing {needle:?}:\n{text}");
        }

        // The receipts the cut reads, filed for the candidate, from its compiler.
        let tree = repo.run(&["rev-parse", &format!("{candidate}^{{tree}}")]);
        fs::write(
            repo.receipts()
                .join(format!("{RECEIPT_MEASURE_PREFIX}{candidate}")),
            format!(
                "{RECEIPT_MAGIC}\nhead {candidate}\ntree {tree}\nmode measure\nscope workspace\n\
                 verdict PASS\nmerge-contract no\nmeasured yes\nskipped none\n\
                 toolchain {CUT_TC}\nbuild-env none\nbuild-config none\nwhen 1\n"
            ),
        )
        .unwrap();
        fs::write(
            repo.receipts()
                .join(format!("{RECEIPT_TRUST_PREFIX}{tree}")),
            format!(
                "{TRUST_RECEIPT_MAGIC}\ntree {tree}\nhead {candidate}\ntrustc {}\nverdict PASS\n\
                 total 5/20\ngated 3\nnot-verified none\nrebase-pending none\n\
                 date 2026-09-29T00:00:00Z\n",
                trustc_commit_of(CUT_TC)
            ),
        )
        .unwrap();
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
        // Another compiler than the one the receipts name: both receipt gates
        // refuse, naming both; no compiler at all: both refuse, saying why.
        let other = "/store/trust/9100/bin trustc 0000000000000000000000000000000000000000";
        let found = check(&candidate, Ok(other.to_string()));
        assert_eq!(found.refused(), ["MEASURE tier", "TRUST lane"]);
        assert!(
            found.transcript().contains(&format!(
                "was measured with `{CUT_TC}`, and this cut builds with `{other}`"
            )),
            "{}",
            found.transcript()
        );
        let found = check(&candidate, Err("no Trust toolchain here".to_string()));
        assert_eq!(found.refused(), ["MEASURE tier", "TRUST lane"]);
        assert!(found.transcript().contains("no Trust toolchain here"));
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

    /// `ship check` answers only with the commit's OWN cutter (2026-09-29
    /// review): the check a shared checkout builds is whatever that checkout
    /// holds, and a checker whose rules are not the judged commit's could say
    /// READY for a commit whose own cutter then refuses. The cut's identity rule
    /// decides, first: a check built from the commit, or from a commit whose
    /// cutter source closure is byte-identical, answers; any other REFUSES TO
    /// ANSWER — an error, before any gate, naming how to ask with the commit's
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
        // Built from a commit whose cutter sources are the judged commit's: it
        // is that commit's cutter, and says so as its first line.
        let docs = repo.commit("a doc", &[("docs/NOTE.md", "prose\n")]);
        let found = precheck_with(&git, &docs, &candidate, tc()).expect("identical closure");
        assert_eq!(found.gates[0].name, "cutter identity");
        let Ok(line) = &found.gates[0].outcome else {
            panic!("{}", found.transcript());
        };
        assert!(
            line.starts_with(&format!(
                "cutter identity: built from {candidate}, tree is at {docs} — nothing under \
                 the cutter's own source closure moved"
            )),
            "{line}"
        );
        // NEGATIVE CONTROLS: other rules answer nothing. A checker built before
        // the judged commit changed them...
        let rules = repo.commit(
            "new rules",
            &[("crates/aterm-release/src/gates.rs", "// other rules\n")],
        );
        let refused = |stamp: &str| {
            precheck_with(&git, &rules, stamp, tc())
                .expect_err("another tree's rules answer nothing")
                .to_string()
        };
        let why = refused(&candidate);
        for needle in [
            &format!(
                "ship check will not answer for {rules}: this aterm-release binary was built \
                 from {candidate}"
            ),
            "1 of the cutter's own source files moved between them \
             (crates/aterm-release/src/gates.rs)",
            &format!(
                "fix:  ask with the commit's own cutter — `PUB_SOURCE_COMMIT={rules} \
                 publish/pre-promote`"
            ),
        ] {
            assert!(why.contains(needle), "missing {needle:?}:\n{why}");
        }
        // ...one that names no commit, or a dirty source closure...
        assert!(refused("unknown").contains("carries no build commit"));
        assert!(refused(DIRTY_BUILD_COMMIT).contains("dirty repository source closure"));
        // ...and one built on another line of history.
        repo.run(&["checkout", "-q", "--orphan", "elsewhere"]);
        let foreign = repo.commit("another history", &[("Cargo.toml", cargo)]);
        let why = refused(&foreign);
        assert!(why.contains("are not one line of history"), "{why}");
        // The commit's own cutter answers.
        assert_eq!(
            precheck_with(&git, &rules, &rules, tc())
                .expect("its own")
                .gates[0]
                .outcome,
            Ok(format!(
                "cutter identity: this check was built from {} itself",
                short(&rules)
            ))
        );
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
        let report = receipt_report(&repo.git(), &repo.receipts()).unwrap();
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
        let report = receipt_report(&repo.git(), &repo.receipts()).unwrap();
        assert_eq!(report.ungated.len(), 3);
        assert_eq!(report.head_verdict.as_deref(), Some("FAIL"));
        assert_eq!(report.head_receipt, Some(repo.receipts().join(&d)));

        // No receipt anywhere: everything scanned is ungated, and none is claimed.
        fs::remove_dir_all(repo.receipts()).unwrap();
        let report = receipt_report(&repo.git(), &repo.receipts()).unwrap();
        assert_eq!(report.newest_gated, None);
        assert_eq!(report.ungated.len(), 4);
        assert_eq!(report.scanned, 4);
        assert_eq!(report.head_receipt, None);
    }

    /// THE SAME BYTES UNDER A NEW COMMIT ID ARE GATED BY THEIR TREE (2026-09-26). A
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
        let report = receipt_report(&repo.git(), &repo.receipts()).unwrap();
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
        let report = receipt_report(&repo.git(), &repo.receipts()).unwrap();
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
        let report = receipt_report(&repo.git(), &repo.receipts()).unwrap();
        assert_eq!(
            report.newest_gated,
            Some((rebased[..9].to_string(), "gated, rebased".to_string()))
        );
        assert_eq!(report.gated_by_tree.as_deref(), Some(&gated[..9]));

        // A sibling worktree at the rebased commit reads the same store and the same
        // answer.
        let linked = repo.root.join("linked");
        repo.run(&[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
            &rebased,
        ]);
        let sibling = GitCli::new(linked);
        let store = receipt_store(&sibling).unwrap();
        assert_eq!(
            store.canonicalize().unwrap(),
            repo.receipts().canonicalize().unwrap(),
            "one store for every worktree"
        );
        let report = receipt_report(&sibling, &store).unwrap();
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
        let report = receipt_report(&repo.git(), &repo.receipts()).unwrap();
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
        let report = receipt_report(&repo.git(), &repo.receipts()).unwrap();
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
        let report = receipt_report(&repo.git(), &repo.receipts()).unwrap();
        assert_eq!(report.ungated.len(), 3, "{report:?}");
        assert_eq!(report.newest_gated, None);
    }
}
