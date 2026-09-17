// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pre-claim gates (release spec §6 `gates.rs`, plus the changelog gates of
//! §3): macOS arm64 host, clean tree, on main, HEAD == origin/main, tag absent
//! local+remote, changelog non-empty/no-`'''`, `gh auth status`, Trust rustc
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

/// The Trust toolchain's stage2 tool dir (`targo`, `trustc`, `trustdoc`, …).
/// `TRUST_STAGE2_BIN` overrides (same contract as tools/verify.sh); the atpkg
/// store's `store/trust/current/bin` (`aterm pkg install trust`) is the default,
/// with `$HOME/trust/build/host/stage2/bin` — a from-source build — behind it.
/// Resolved to the PHYSICAL path: Trust's
/// `build/host` is commonly a target-triple symlink and the protected Trust
/// drivers refuse a symlinked toolchain path. The gates resolve the trust-named
/// binaries directly rather than a PATH `cargo` — correctness does not depend
/// on the operator's rustup state. (An earlier revision of this comment claimed
/// the stock-name `{rustc,cargo}` compatibility entries were purged from stage2;
/// current stage2 builds ship them again, and the rustup `trust` link over the
/// stage2 dir is exactly what makes `cargo ship …` dispatch into Trust — the
/// front door `provision` audits. The gates still never rely on it.)
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
    // Resolution order, first hit wins. No step requires anyone to remember an
    // environment variable: a toolchain installed by `atpkg install trust` is found
    // automatically, which is the ordinary way to get one.
    //
    //   1. $TRUST_STAGE2_BIN — an explicit override for an unusual location. This is
    //      a LOCATION knob, not a trust knob: it cannot change what anything trusts,
    //      only where the compiler is found.
    //   2. the atpkg store, resolved exactly as atpkg resolves it (so a configured
    //      `[packages].prefix` — e.g. a root-owned system prefix — is honoured).
    //   3. $HOME/trust/build/host/stage2/bin — a toolchain built from source with x.py,
    //      the developer alternative to the store.
    let candidates = trust_stage2_candidates();
    let mut tried = Vec::new();
    for dir in candidates {
        match fs::canonicalize(&dir) {
            Ok(resolved) if resolved.join("trustc").is_file() => return Ok(resolved),
            _ => tried.push(dir.display().to_string()),
        }
    }
    // FAULT ONLY, plus the one fact the operator cannot derive: where it looked. The
    // three remedies live in [`TRUST_TOOLCHAIN_REMEDIES`] so that the caller can lay them
    // out under its own `fix:` / `or:` — `provision::toolchain_check` used to pass this
    // whole message through as the FAULT and then add a fourth remedy beside it, which
    // is how one fixable problem started looking like two.
    Err(Error::new(format!(
        "no Trust toolchain found\nlooked in: {}",
        tried.join(", ")
    )))
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
     or, from source: python3 x.py build --stage 2, in $HOME/trust\n\
     or point TRUST_STAGE2_BIN at an existing stage2 bin dir";

/// The ordered places a Trust toolchain may live. Split out so the order is readable
/// and testable without a filesystem.
fn trust_stage2_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(explicit) = env::var_os("TRUST_STAGE2_BIN") {
        out.push(PathBuf::from(explicit));
    }
    // The atpkg store, via atpkg's own config + prefix validation.
    let home = aterm_types::dirs::home_dir();
    let configured = atpkg::config::load().prefix_path(home.as_deref());
    if let Some(layout) = atpkg::store::resolve(configured.as_deref()) {
        out.push(layout.program_current("trust").join("bin"));
    }
    if let Ok(home) = env::var("HOME") {
        out.push(Path::new(&home).join("trust/build/host/stage2/bin"));
    }
    out
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
             (`aterm pkg install trust`, then `aterm pkg doctor`), or from source rebuild it \
             (`python3 x.py build --stage 2` in $HOME/trust), or point TRUST_STAGE2_BIN at a \
             stage2 bin dir that carries targo",
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
    /// Recut (spec §5): the notes were already rolled into `## [version]` by
    /// the earlier wedged cut, so the changelog gate judges THAT section — the
    /// fresh `[Unreleased]` scaffold above it is legitimately empty.
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
}

/// Run every gate, in the transcript's order, first failure wins. Cheap and
/// side-effect-free by construction (the one network touch is a fetch): this
/// is the always-on preflight — the optional deep gate (`--gate` →
/// tools/verify.sh --full) layers on top in chunk C, never replaces this.
pub fn run_all(git: &dyn GitRunner, repo: &Path, opts: &GateOpts) -> Result<GateReport> {
    host_gate()?;
    clean_tree(git)?;
    on_main(git)?;
    let head = head_matches_origin(git)?;
    // The tree is proven; now prove the binary proving it.
    cutter_identity_gate(&head, opts.allow_stale_cutter)?;
    tag_free(git, &opts.version)?;
    let cl = changelog_gate(
        repo,
        if opts.recut {
            &opts.version
        } else {
            "Unreleased"
        },
    )?;
    let gh_account = gh_auth()?;
    locked_metadata_gate(repo)?;
    let trustc = trustc_probe(repo)?;
    // The compiler runs; now prove it will not tag everything it writes.
    provenance_gate(&trustc)?;
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
    let free_disk_gib = disk_gate(repo)?;
    // Last, because it is the only gate that talks to the public channel: the cheap
    // local refusals should all have fired before we spend a network round trip.
    let channel_version = channel_version_gate(repo, &opts.version, opts.offline)?;
    Ok(GateReport {
        head_short: head.chars().take(8).collect(),
        changelog_entries: cl.entries,
        gh_account,
        trustc,
        universal,
        free_disk_gib,
        channel_version,
    })
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

/// On main: the claim pushes to origin/main; cutting from any other branch
/// would either fail the push or, worse, publish a side branch's tree.
pub fn on_main(git: &dyn GitRunner) -> Result<()> {
    let branch = git_ok(git, &["rev-parse", "--abbrev-ref", "HEAD"])?.stdout_utf8();
    let branch = branch.trim();
    if branch == "main" {
        Ok(())
    } else {
        Err(Error::new(format!(
            "on branch {branch:?} — releases are cut only from main"
        )))
    }
}

/// Fetch + require HEAD == origin/main (spec §2 step 1, surfaced early as a
/// gate so it costs seconds). Fail closed when offline: no offline cuts — the
/// ledger claim IS a push. Returns the HEAD sha for the banner.
pub fn head_matches_origin(git: &dyn GitRunner) -> Result<String> {
    git_ok(git, &["fetch", "origin", "main"])
        .map_err(|e| Error::new(format!("cannot reach origin (no offline cuts): {e}")))?;
    let head = rev_parse(git, "HEAD")?;
    let origin_tip = rev_parse(git, "origin/main")?;
    if head != origin_tip {
        return Err(Error::new(format!(
            "HEAD ({head}) != origin/main ({origin_tip}) — pull first"
        )));
    }
    Ok(head)
}

/// The commit THIS BINARY was built from, stamped by `build.rs`; the reserved
/// null object ID means its repository source closure was dirty, and
/// `"unknown"` means the build could not establish either fact.
pub const BUILD_COMMIT: &str = env!("ATERM_RELEASE_BUILD_COMMIT");
const DIRTY_BUILD_COMMIT: &str = "0000000000000000000000000000000000000000";

/// Prove the cutter is the tree's own — the binary-side twin of
/// [`head_matches_origin`].
///
/// Every other pre-claim gate proves something about the TREE and nothing about
/// the binary doing the proving, which is the hole v0.63.0 fell through: a
/// cutter built from an older tree cut a seeded 1.07 GB image plus an
/// `-x86_64.dmg` from source that had been lean since 52c1936f, validated that
/// output against its OWN older `required_asset_names`, and passed. The tree was
/// clean, on main, and equal to origin/main the whole time. `cargo clean -p
/// aterm-release` in the runbook is the manual version of this check; a runbook
/// step is not a gate.
///
/// Every remote-mutating cut must match. Only `--dry-run` may proceed with a
/// mismatch, because it stops before any upload; the mismatch is still
/// reported so its local result is not mistaken for evidence about the tree.
/// A rehearsal publishes to a scratch repository and therefore gets no
/// exemption.
pub fn cutter_identity_gate(head: &str, allow_stale: bool) -> Result<()> {
    match cutter_identity_verdict(BUILD_COMMIT, head, allow_stale) {
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
    allow_stale: bool,
) -> Result<Option<String>> {
    if stamp == head {
        return Ok(None);
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
        format!("this aterm-release binary was built from {stamp}, but the tree is at {head}")
    };
    if allow_stale {
        return Ok(Some(format!(
            "cutter identity: {what} — allowed because this dry-run publishes nowhere,              but it is running OTHER code than the tree"
        )));
    }
    Err(Error::new(format!(
        "{what}.
fix:  cargo clean -p aterm-release   (then re-run; the cutter is rebuilt from this tree)
         why:  v0.63.0 was cut by a binary older than its own source and shipped the seeded          image the tree had already retired"
    )))
}

/// Apply the real-mutation cutter check to the checkout the caller is about
/// to act from. Resume, recovery, abandon, retire and yank do not enter
/// [`run_all`], so each of those paths uses this shared boundary before its
/// first remote mutation.
pub fn current_cutter_identity_gate(git: &dyn GitRunner) -> Result<()> {
    cutter_identity_gate(&rev_parse(git, "HEAD")?, false)
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
///   MEASURED by writing a probe file and reading the attribute back; `xattr -d` cannot
///   remove the tag, so nothing this gate could do would fix it, and it says what does.
///
/// A path this gate cannot inspect is a refusal, not a pass: the tag's whole failure mode
/// is being invisible until after the claim.
pub fn provenance_gate(trustc: &Path) -> Result<()> {
    let targo = trust_stage2_bin()?.join("targo");
    let cutter = env::current_exe().and_then(fs::canonicalize).map_err(|e| {
        Error::new(format!(
            "provenance gate: cannot resolve the cutter's own binary: {e}"
        ))
    })?;
    let mut carriers: Vec<(&'static str, PathBuf)> = Vec::new();
    for (label, path) in [
        ("trustc", trustc.to_path_buf()),
        ("targo", targo),
        ("the cutter's own binary", cutter),
    ] {
        match atpkg::provenance::xattr_names(&path) {
            Ok(names)
                if names
                    .iter()
                    .any(|n| n == atpkg::provenance::PROVENANCE_XATTR) =>
            {
                carriers.push((label, path));
            }
            Ok(_) => {}
            Err(e) => {
                return Err(Error::new(format!(
                    "provenance gate: cannot inspect {label} at {} for com.apple.provenance: {e}",
                    path.display()
                )));
            }
        }
    }
    // THE DYLIBS THE COMPILER LOADS (2026-09-15): a process that `dlopen`s a tagged
    // library becomes tracked — measured with a launchd-spawned python loading a tagged
    // copy of the bundle's `libstd` — so a clean `trustc` over a tagged `lib/` writes
    // tagged files all the same. Every dylib under the bundle's `lib/` is a carrier
    // candidate; the first few are named, the count says the rest.
    if let Some(lib) = trustc
        .parent()
        .and_then(Path::parent)
        .map(|b| b.join("lib"))
        && lib.is_dir()
    {
        let scan = atpkg::provenance::tagged_files_under(&lib, atpkg::provenance::PROVENANCE_XATTR);
        let dylibs: Vec<PathBuf> = scan
            .carriers
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "dylib"))
            .collect();
        if !dylibs.is_empty() {
            let shown = dylibs.len().min(3);
            for path in dylibs.iter().take(shown) {
                carriers.push(("a library trustc loads", path.clone()));
            }
            if dylibs.len() > shown {
                carriers.push((
                    "…and more libraries under the bundle's lib/ (count in the message)",
                    lib.join(format!("({} tagged dylibs in all)", dylibs.len())),
                ));
            }
        }
    }
    let scratch = env::temp_dir();
    let tracked = atpkg::provenance::measure_tracked(&scratch).ok_or_else(|| {
        Error::new(format!(
            "provenance gate: could not write a probe file under {} to measure whether this \
             cutter process is provenance-tracked",
            scratch.display()
        ))
    })?;
    provenance_verdict(&carriers, tracked)
}

/// The decision behind [`provenance_gate`], without the filesystem: `carriers` are the
/// `(label, path)` pairs found tagged, `cutter_tracked` is the probe-write measurement.
/// Clean on both counts passes silently; anything else is the one refusal, naming every
/// carrier, what the tag does, and the two ways out.
pub fn provenance_verdict(carriers: &[(&str, PathBuf)], cutter_tracked: bool) -> Result<()> {
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
    if cutter_tracked {
        msg.push_str(
            "  this cutter PROCESS is provenance-tracked: a probe file it wrote came back \
             tagged — it runs under a tracked parent (a shell inside a tracked aterm.app, or an \
             agent started from a tagged binary such as a natively installed `claude`), so even \
             an untagged toolchain would write tagged files from here\n",
        );
    }
    msg.push_str(atpkg::provenance::WHAT_IT_BREAKS);
    msg.push_str("\nfix:  ");
    msg.push_str(atpkg::provenance::REMEDY);
    msg.push_str(
        "\nor:   with the toolchain clean, run this cut as a launchd job — `launchctl submit \
         -l aterm-cut -- <path to cargo-ship> ship cut …` — so the cutter is neither a \
         descendant of aterm.app nor of an agent (docs/RELEASING.md)",
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
mod cutter_identity_tests {
    use super::*;

    const HEAD: &str = "2617295d1111111111111111111111111111aaaa";
    const OLDER: &str = "15f2b5b6bc51d9ef56e5e4fa50f434fca07f40ee";

    #[test]
    fn the_trees_own_binary_passes_silently() {
        assert!(matches!(
            cutter_identity_verdict(HEAD, HEAD, false),
            Ok(None)
        ));
    }

    /// The v0.63.0 shape: a real cut by a binary older than its own source.
    #[test]
    fn a_stale_binary_cannot_cut_for_real() {
        let err = cutter_identity_verdict(OLDER, HEAD, false)
            .expect_err("a stale cutter must not cut a real release");
        let msg = err.to_string();
        assert!(msg.contains(OLDER), "{msg}");
        assert!(msg.contains(HEAD), "{msg}");
        assert!(msg.contains("cargo clean -p aterm-release"), "{msg}");
        assert!(
            msg.contains("v0.63.0"),
            "the refusal should say why it exists: {msg}"
        );
    }

    /// "Cannot tell" must refuse, not pass — an unstamped binary is exactly as
    /// unproven as a stale one.
    #[test]
    fn an_unstamped_binary_fails_closed() {
        let err = cutter_identity_verdict("unknown", HEAD, false)
            .expect_err("an unstamped cutter must fail closed");
        assert!(err.to_string().contains("no build commit"), "{err}");
    }

    #[test]
    fn a_dirty_built_binary_fails_closed() {
        let err = cutter_identity_verdict(DIRTY_BUILD_COMMIT, HEAD, false)
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
        let note = cutter_identity_verdict(OLDER, HEAD, true)
            .expect("a dry-run must not be blocked")
            .expect("a mismatched dry-run must say something");
        assert!(note.contains("dry-run publishes nowhere"), "{note}");
        assert!(note.contains("OTHER code"), "{note}");
        assert!(
            cutter_identity_verdict("unknown", HEAD, true).is_ok(),
            "an unstamped dry-run is not blocked either"
        );
        // ...and a matching dry-run stays silent.
        assert!(matches!(
            cutter_identity_verdict(HEAD, HEAD, true),
            Ok(None)
        ));
    }

    #[test]
    fn a_stale_rehearsal_is_refused_because_it_mutates_a_remote() {
        let err = cutter_identity_verdict(OLDER, HEAD, false)
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
        assert!(provenance_verdict(&[], false).is_ok());
    }

    /// The v0.83.0 shape: a tagged trustc. The refusal names the path, the attribute,
    /// the post-claim failure it pre-empts, and both remedies.
    #[test]
    fn a_tagged_trustc_is_refused_before_the_claim_with_the_remedies() {
        let trustc = PathBuf::from(
            "/Users//me/Library/Application Support/aterm/pkg/store/trust/8590/bin/trustc",
        );
        let err = provenance_verdict(&[("trustc", trustc.clone())], false)
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
        assert!(msg.contains("TRUST_STAGE2_BIN"), "remedy 1: {msg}");
        assert!(
            msg.contains("aterm pkg uninstall <program> && aterm pkg install <program>"),
            "remedy 2: {msg}"
        );
        assert!(msg.contains("launchctl submit"), "remedy 3: {msg}");
        assert!(
            !msg.contains("cutter PROCESS is provenance-tracked"),
            "not measured tracked: {msg}"
        );
    }

    /// A clean toolchain does not save a tracked cutter process: the tag follows the
    /// parent too, and the probe write is what shows it.
    #[test]
    fn a_tracked_cutter_process_is_refused_even_with_a_clean_toolchain() {
        let err = provenance_verdict(&[], true).expect_err("a tracked cutter must not cut");
        let msg = err.to_string();
        assert!(
            msg.contains("cutter PROCESS is provenance-tracked"),
            "{msg}"
        );
        assert!(msg.contains("probe file"), "{msg}");
        assert!(msg.contains("launchctl submit"), "{msg}");
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
