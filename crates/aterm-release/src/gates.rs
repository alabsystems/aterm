// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pre-claim gates (release spec §6 `gates.rs`, plus the changelog gates of
//! §3): macOS arm64 host, clean tree, HEAD == the published commit (a real
//! cut — [`place_published`] put the cut tree there), tag absent local+remote,
//! changelog non-empty/no-`'''`, `gh auth status`, Trust rustc
//! probe (always on — the repo compiles with Trust, there is no opt-out
//! lane), x86_64 rustup target probe with printed remediation (`--arm64-only`
//! opt-out), disk space. All fail closed BEFORE anything is committed or
//! pushed — a failed gate costs seconds, not a burned ledger number.
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
use crate::ledger::{Error, GitRunner, Result, git_ok, rev_parse};
use crate::mirror;

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
/// `PATH`. A candidate must carry `targo` AND `trustc`. No build tree is probed: the
/// `$HOME/trust/build/host/stage2/bin` fallback was deleted 2026-09-24 (retired from the
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
    pin_first_success(&PINNED, resolve_trust_stage2_bin)
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
    let mut out = Vec::new();
    if let Some(rustup) = atpkg::seam::rustup_home() {
        let entry = atpkg::seam::seam_path(&rustup, atpkg::seam::DEFAULT_SEAM);
        out.extend(rustup_entry_source(&entry, layout.as_ref()).map(|d| d.join("bin")));
    }
    if let Some(layout) = &layout {
        out.push(layout.program_current("trust").join("bin"));
    }
    if let Some(path) = env::var_os("PATH") {
        out.extend(env::split_paths(&path));
    }
    (false, out)
}

/// The sysroot the rustup `trust` entry stands for. atpkg's VIEW (`<prefix>/rustup/…`)
/// is rebuilt in place on every update, so it answers with what the view presents —
/// the store's live build, or the dev-linked checkout — whose physical path cannot
/// change under a running cut; any other entry (a hand link, the store itself) is
/// itself. `None` when the view presents nothing atpkg can use.
fn rustup_entry_source(entry: &Path, layout: Option<&atpkg::store::Layout>) -> Option<PathBuf> {
    let resolved = fs::canonicalize(entry).ok()?;
    let Some(layout) = layout else {
        return Some(resolved);
    };
    let views = fs::canonicalize(atpkg::seam::views_root(layout)).ok();
    if !views.is_some_and(|v| resolved.starts_with(v)) {
        return Some(resolved);
    }
    match atpkg::seam::view_source(layout) {
        atpkg::seam::ViewSource::Store => Some(atpkg::seam::store_current(layout)),
        atpkg::seam::ViewSource::Linked(checkout) => Some(checkout),
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
    let gh_account = gh_auth()?;
    locked_metadata_gate(tree)?;
    let trustc = trustc_probe(tree)?;
    // The compiler runs; now prove it will not tag everything it writes.
    provenance_gate(&trustc)?;
    // ...and that the scheduler will not starve the one proof that runs after the
    // claim.
    launchd_qos_gate(opts.paint_smoke)?;
    let universal = if opts.arm64_only {
        false
    } else {
        // The cut appends the remedies itself: the probe returns a fault, and this is
        // the only caller that must STOP, so it is the only one that has to say what to
        // do about it right here.
        x86_target_probe().map_err(|e| {
            Error::new(format!(
                "{e}\nfix:  {}",
                X86_SLICE_REMEDIES.replace('\n', "\n      ")
            ))
        })?;
        true
    };
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
    Ok(GateReport {
        head_short: head.chars().take(8).collect(),
        changelog_entries: cl.entries,
        gh_account,
        trustc,
        universal,
        free_disk_gib,
        channel_version,
        processes_checked,
        published: opts.published.clone(),
        receipts,
    })
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
    /// The newest first-parent commit the push gate's predicate admits as gated, as
    /// `(short sha, subject)`; `None` when none was found within the scan.
    pub newest_gated: Option<(String, String)>,
    /// The first-parent commits above it, newest first, as `short subject`.
    pub ungated: Vec<String>,
    /// How many first-parent commits were scanned (the scan is bounded).
    pub scanned: usize,
    /// HEAD's own receipt verdict when it has one (`PASS` / `FAIL` / `COULD-NOT-RUN`).
    pub head_verdict: Option<String>,
}

/// How many first-parent commits [`receipt_report`] reads before it stops looking.
pub const RECEIPT_SCAN_LIMIT: usize = 2000;

/// The first line of every gate receipt (`crates/aterm-verify/src/receipt.rs`
/// `MAGIC`); a file that does not start with it — an older format included — is
/// no receipt at all.
const RECEIPT_MAGIC: &str = "aterm-verify receipt 2";

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

/// The push gate's `full_pass`: a receipt (the gate writes one only for a clean
/// tree) that discharged the whole merge contract (`.githooks/pre-push`). An
/// unreadable file is not a pass.
fn receipt_full_pass(receipts: &Path, sha: &str) -> bool {
    fs::read_to_string(receipts.join(sha))
        .is_ok_and(|text| receipt_field(&text, "merge-contract").as_deref() == Some("yes"))
}

/// THE UNGATED RANGE, STATED (2026-09-23). Walks HEAD's first-parent history to the
/// newest commit the push gate's own predicate admits as gated — a passing receipt
/// (merge contract discharged), or a clean automatic merge of a receipted
/// side (two parents, one of them receipted, and the tree byte-equal to
/// `git merge-tree --write-tree` of the two) — and reports how many commits sit
/// above it. The same predicate `.githooks/pre-push` applies, so "gated" means here
/// what it means at push time.
///
/// STATED, NOT REQUIRED. A receipt for the exact HEAD is a race the gate loses by
/// construction (it takes an hour, peers push every few minutes — pre-push's own
/// header), so demanding one would refuse every cut. What this buys is that the
/// number is on the transcript: 0.91 was cut 136 commits past the newest receipt
/// and nothing said so. On a real cut HEAD is the published commit, so the count
/// is the published commit's.
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
            "--format=%H%x1f%P%x1f%s",
            "HEAD",
        ],
    )?
    .stdout_utf8();
    let mut above = Vec::new();
    let mut head_verdict = None;
    let mut scanned = 0;
    for (index, line) in walk.lines().enumerate() {
        let mut fields = line.splitn(3, '\u{1f}');
        let (Some(sha), Some(parents), subject) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let subject = subject.unwrap_or_default();
        scanned += 1;
        if index == 0 {
            head_verdict = fs::read_to_string(receipts.join(sha))
                .ok()
                .and_then(|text| receipt_field(&text, "verdict"));
        }
        let parents: Vec<&str> = parents.split_whitespace().collect();
        let gated = receipt_full_pass(receipts, sha)
            || (parents.len() == 2
                && (receipt_full_pass(receipts, parents[0])
                    || receipt_full_pass(receipts, parents[1]))
                && clean_automatic_merge(git, sha, parents[0], parents[1])?);
        if gated {
            return Ok(ReceiptReport {
                newest_gated: Some((short(sha).to_string(), subject.to_string())),
                ungated: above,
                scanned,
                head_verdict,
            });
        }
        above.push(format!("{} {subject}", short(sha)));
    }
    Ok(ReceiptReport {
        newest_gated: None,
        ungated: above,
        scanned,
        head_verdict,
    })
}

/// Whether `merge`'s tree is git's own clean merge of its two parents — nothing
/// resolved or added by hand (pre-push's `gated_merge`).
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

/// Prove the public channel's source tree already carries the version being cut.
///
/// The pure comparison is [`mirror::check_channel_version`]; this is only its I/O
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
    let Some(slug) = mirror::update_channel_slug(&local)? else {
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
    match mirror::check_channel_version(version, &body)? {
        mirror::ChannelVersion::Agrees => Ok(Some(version.to_string())),
        mirror::ChannelVersion::NoManifest => Ok(None),
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
/// machine via the `cargo ship` run alias — never cross-compiled, never
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
/// clean the whole time. `cargo clean -p
/// aterm-release` in the runbook is the manual version of this check; a runbook
/// step is not a gate.
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
    let what = if stamp == "unknown" {
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

/// Apply the real-mutation cutter check to the checkout the caller is about
/// to act from. Resume, recovery, abandon, retire and yank do not enter
/// [`run_all`], so each of those paths uses this shared boundary before its
/// first remote mutation.
pub fn current_cutter_identity_gate(git: &dyn GitRunner) -> Result<()> {
    cutter_identity_gate(git, &rev_parse(git, "HEAD")?, false)
}

/// Tag vX.Y.Z must be absent BOTH locally and on origin: the publish step mints
/// it late (spec decision 5), so any pre-existing tag means this version was
/// already cut (or half-cut) — colliding with it would re-point a published
/// artifact. Local and remote are checked separately because either alone can
/// be stale.
pub fn tag_free(git: &dyn GitRunner, version: &str) -> Result<()> {
    let tag = format!("v{version}");
    // Local: `rev-parse -q --verify` exits 0 iff the ref EXISTS — existence is
    // the failure here, so this is the one git call whose non-zero exit is the
    // happy path.
    let local = git.git(&["rev-parse", "-q", "--verify", &format!("refs/tags/{tag}")])?;
    if local.success() {
        return Err(Error::new(format!(
            "tag {tag} already exists locally — this version was already cut (bump \
             [workspace.package] version's MINOR in Cargo.toml, or delete the stale \
             tag if that cut was abandoned)"
        )));
    }
    let remote = git_ok(
        git,
        &["ls-remote", "--tags", "origin", &format!("refs/tags/{tag}")],
    )?;
    if !remote.stdout_utf8().trim().is_empty() {
        return Err(Error::new(format!(
            "tag {tag} already exists on origin — v{version} was already cut/published \
             elsewhere; bump [workspace.package] version's MINOR in Cargo.toml"
        )));
    }
    Ok(())
}

/// Changelog gates (spec §3), delegated to changelog.rs: the section's real
/// body non-empty, no `'''`. `section` is "Unreleased" for a fresh cut, the
/// version itself for a recut (see [`GateOpts::recut`]).
pub fn changelog_gate(repo: &Path, section: &str) -> Result<changelog::GateSummary> {
    let path = repo.join(changelog::CHANGELOG_FILE);
    let text = fs::read_to_string(&path)
        .map_err(|e| Error::new(format!("cannot read {}: {e}", path.display())))?;
    if section == "Unreleased" {
        changelog::gate_unreleased(&text)
    } else {
        changelog::gate_section(&text, section)
    }
}

/// `gh auth status` must succeed — the publish half of the cut is all `gh`,
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
/// Trust lane's rustflags, in lookup order. The live one is FIRST and is not a
/// triple: ecb1d6691 (2026-08-30) replaced the two per-triple copies with one
/// `[target.'cfg(trust_verify)']` table scoped to the COMPILER that understands
/// the flag rather than to a host, so the single table now reaches every Trust
/// lane on every target. The two retired triples stay behind it only so an older
/// checkout still reads its own config.
const TRUST_LANE_TABLES: [&str; 3] = [
    "cfg(trust_verify)",
    "aarch64-apple-darwin",
    "x86_64-unknown-linux-gnu",
];

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
pub fn launchd_qos_gate(paint_smoke_will_run: bool) -> Result<()> {
    let tier = match env::var("XPC_SERVICE_NAME").ok().filter(|l| l != "0") {
        None => SpawnTier::Shell,
        Some(label) => read_spawn_tier(&label),
    };
    match launchd_qos_verdict(&tier, paint_smoke_will_run)? {
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
/// is something the transcript should say, `Err` is the refusal.
pub fn launchd_qos_verdict(tier: &SpawnTier, paint_smoke_will_run: bool) -> Result<Option<String>> {
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
        SpawnTier::Other(name) => Err(Error::new(format!(
            "this cut is a launchd job spawned at tier {name:?}, not `interactive` — the \
             paint smoke samples at 50 ms and its timer fires 25-75 ms late at this tier; \
             measured, 11 of 30 takes go red for that reason alone, at the self-check step \
             AFTER the ledger claim (docs/RELEASING.md, 2026-09-12).\n\
             fix:  bootstrap the job from a plist carrying \
             <key>ProcessType</key><string>Interactive</string> (docs/RELEASING.md has the \
             plist), or cut from an interactive shell. `launchctl submit` cannot set the \
             tier and is the shape that produced the measurement above.\n\
             or:   --no-paint-smoke, which is an EMERGENCY escape and refuses on a \
             notarized real cut without its acknowledgement"
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
pub fn provenance_gate(trustc: &Path) -> Result<()> {
    provenance_gate_with(trustc, atpkg::provenance::heal)
}

/// [`provenance_gate`] with the toolchain heal explicit ([`atpkg::provenance::Healer`]).
/// A cut passes `atpkg::provenance::heal`; a test that runs the gate over this machine's
/// INSTALLED toolchain passes one that changes nothing, so a test run never rewrites the
/// store (the heal itself is tested on a scratch toolchain).
pub fn provenance_gate_with(trustc: &Path, heal: atpkg::provenance::Healer) -> Result<()> {
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
    provenance_verdict(&carriers, tracked, healed.why())
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
/// what the tag does, and the ways out.
pub fn provenance_verdict(
    carriers: &[(&str, PathBuf)],
    cutter_tracked: bool,
    heal_left: Option<&str>,
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
    msg.push_str("\nfix:  ");
    msg.push_str(atpkg::provenance::REMEDY);
    msg.push_str(
        // The remedy names the LAUNCHER, not the plist it writes. This message is
        // what sent every cut of this session to QOS_CLASS_UTILITY: it used to read
        // `launchctl submit`, an operator pasted exactly that, and the post-claim
        // paint smoke was starved by the tier it chose. A refusal that hands over a
        // runnable command is the whole fix — `tools/cut-launch.sh` bootstraps the
        // ProcessType=Interactive agent itself, so nobody has to transcribe a plist.
        "\nor:   with the toolchain clean, run this cut as a launchd job — \
         `tools/cut-launch.sh --release-credentials <path>` — so the cutter is neither a \
         descendant of aterm.app nor of an agent. Use THAT launcher and not `launchctl \
         submit`: submit escapes this tag and runs the job at QOS_CLASS_UTILITY, which the \
         paint smoke's aterm inherits and starves under (11 of 30 takes red; \
         docs/RELEASING.md)",
    );
    Err(Error::new(msg))
}

/// The x86_64 compat slice builds on upstream stable (buildplan.rs pins
/// `RUSTUP_TOOLCHAIN=stable`), so probe STABLE's installed targets — a bare
/// `rustup target list` would resolve the repo's `trust` toolchain via
/// rust-toolchain.toml, and custom toolchains never carry rustup-managed
/// targets. When the target is absent, print the exact remediation and
/// require the explicit `--arm64-only` to proceed single-arch (spec decision
/// 18) — never silently ship a thinner artifact than v0.25 did.
pub fn x86_target_probe() -> Result<()> {
    let out = Command::new("rustup")
        .env("RUSTUP_TOOLCHAIN", "stable")
        .args(["target", "list", "--installed"])
        .output()
        .map_err(|e| {
            // Name the escape hatch. rustup is NOT this repo's toolchain — THE
            // toolchain is the Trust stage2 tree — and it is wanted here for
            // exactly one thing: upstream stable's x86_64-apple-darwin std, which
            // Trust does not have. So on a Trust-only machine this is not a broken
            // setup to go fix; it is a choice about what to ship, and the operator
            // needs to be told that rather than sent to install a toolchain manager
            // the repo otherwise refuses. Its twin in buildplan.rs already says so.
            Error::new(format!(
                "failed to run rustup ({e}). The x86_64 compat slice needs upstream \
                 stable's std for that target (Trust has none — the one documented \
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
mod launchd_qos_tests {
    use super::*;

    #[test]
    fn a_shell_and_an_interactive_agent_are_the_calibrated_cases() {
        for tier in [SpawnTier::Shell, SpawnTier::Interactive] {
            assert_eq!(
                launchd_qos_verdict(&tier, true).expect("calibrated tiers pass"),
                None,
                "{tier:?} must pass silently"
            );
        }
    }

    /// `launchctl submit`'s tier, refused BEFORE the claim rather than sampled
    /// after it.
    #[test]
    fn a_utility_job_is_refused_with_the_plist_remedy() {
        let err = launchd_qos_verdict(&SpawnTier::Other("background".to_string()), true)
            .expect_err("a starved tier must not reach the claim");
        let msg = err.to_string();
        assert!(msg.contains("ProcessType"), "{msg}");
        assert!(
            msg.contains("11 of 30"),
            "the refusal carries its measurement: {msg}"
        );
        assert!(
            msg.contains("launchctl submit"),
            "it names the shape that causes it: {msg}"
        );
    }

    /// Cannot-tell is a NOTE, not a refusal — the opposite of the identity gate,
    /// and deliberately: the cost here is a resumable flake, not a bad artifact.
    #[test]
    fn an_unreadable_tier_notes_and_proceeds() {
        let note = launchd_qos_verdict(&SpawnTier::Unknown, true)
            .expect("cannot-tell must not block a release")
            .expect("but it must say something");
        assert!(note.contains("ProcessType=Interactive"), "{note}");
    }

    /// No smoke, nothing to starve.
    #[test]
    fn no_paint_smoke_makes_the_tier_irrelevant() {
        assert_eq!(
            launchd_qos_verdict(&SpawnTier::Other("background".to_string()), false)
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

#[cfg(test)]
mod provenance_gate_tests {
    use super::*;

    /// Clean toolchain, untracked cutter: silent pass.
    #[test]
    fn a_clean_toolchain_under_an_untracked_cutter_passes() {
        assert!(provenance_verdict(&[], false, None).is_ok());
    }

    /// The v0.83.0 shape: a tagged trustc. The refusal names the path, the attribute,
    /// the post-claim failure it pre-empts, and the remedies — the heal (`aterm pkg
    /// repair`, the cutter's own), never a re-install that writes the same tagged files.
    #[test]
    fn a_tagged_trustc_is_refused_before_the_claim_with_the_remedies() {
        let trustc = PathBuf::from(
            "/Users//me/Library/Application Support/aterm/pkg/store/trust/8590/bin/trustc",
        );
        let err = provenance_verdict(&[("trustc", trustc.clone())], false, None)
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
        let err = provenance_verdict(&[], true, None).expect_err("a tracked cutter must not cut");
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
        assert!(msg.contains("tools/cut-launch.sh"), "{msg}");
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
        let err = provenance_verdict(&[("trustc", trustc.clone())], false, Some("xattr exited 1"))
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
        let msg = provenance_verdict(&[], true, Some("xattr exited 1"))
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
            provenance_verdict(&carriers, false, healed.why())
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

    /// An older checkout still reads its own config: the retired per-triple
    /// names stay in the list behind the live one.
    #[test]
    fn a_retired_per_triple_table_is_still_read() {
        let config: aterm_toml::Value =
            "[target.aarch64-apple-darwin]\nrustflags = [\"-Ztrust-verify=off\"]\n"
                .parse()
                .unwrap();
        assert_eq!(
            trust_lane_rustflags(&config),
            Some(vec!["-Ztrust-verify=off".to_string()])
        );
    }

    /// …but where both exist the LIVE table wins, because that is the one cargo
    /// applies on a Trust compiler.
    #[test]
    fn the_live_table_wins_over_a_retired_one() {
        let config: aterm_toml::Value = "[target.'cfg(trust_verify)']\n\
             rustflags = [\"-Ztrust-verify=off\", \"--cfg\", \"clean_islands\"]\n\
             [target.aarch64-apple-darwin]\n\
             rustflags = [\"-Zstale\"]\n"
            .parse()
            .unwrap();
        assert_eq!(
            trust_lane_rustflags(&config),
            Some(vec![
                "-Ztrust-verify=off".to_string(),
                "--cfg".to_string(),
                "clean_islands".to_string(),
            ])
        );
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
    //! receipt, by the push gate's own predicate.

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

    /// A NARROWED PASS IS NOT A GATE, as the push gate reads it: a clean, unskipped
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

    #[test]
    fn the_receipt_report_counts_to_the_newest_gated_commit_by_the_push_predicate() {
        let repo = Repo::new("receipts");
        let gated = repo.commit("gated", &[("a", "1")]);
        repo.receipt(&gated, "PASS", "yes");
        let b = repo.commit("ungated one", &[("a", "2")]);
        // An older gate's receipt, and a narrowed run, do not admit a push — nor
        // count here.
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

        // No receipt anywhere: everything scanned is ungated, and none is claimed.
        fs::remove_dir_all(repo.receipts()).unwrap();
        let report = receipt_report(&repo.git(), &repo.receipts()).unwrap();
        assert_eq!(report.newest_gated, None);
        assert_eq!(report.ungated.len(), 4);
        assert_eq!(report.scanned, 4);
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
