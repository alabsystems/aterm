// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Publish (release spec §7 steps 5–6) + the whole `cut` pipeline: draft-first
//! one direct GitHub REST draft POST targeting the claim sha → upload every asset once by
//! immutable release ID under a durable intent →
//! re-run the pre-flip monotonic check against the client's exact selection
//! rule → push the annotated tag (late — a failed cut never leaves a public
//! tag, spec decision 5) → `--draft=false` flip → metadata-archive every
//! historical exact-name appcast. No
//! client can ever observe a half-uploaded release. Every step is journaled
//! in `dist/cut-state.toml` for `--resume`/recut/abandon (spec §5).
//!
//! One orchestrator ([`run_cut`]) drives all four cut flavors — real, resume,
//! `--dry-run`, `--rehearse` — through the SAME step list, so the rehearsal
//! (spec decision 17) exercises the exact code path of the real cut, minus
//! the ledger push and the origin-mutating steps (tag).

use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use aterm_update_core::Manifest;
use aterm_update_core::tag::TagError;

use crate::ledger::{self, Error, GitCli, GitRunner, Result, RunOut, git_ok, rev_parse};
use crate::{buildplan, bundle, changelog, dmg, gates, manifest_out, mirror, sign, verify};

// ---------------------------------------------------------------------------
// CLI-facing surface
// ---------------------------------------------------------------------------

/// Every `cut` flag (spec §5), parsed by cli.rs. (`PartialEq` exists for the
/// CLI parse table in tests/resume.rs.)
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CutOptions {
    /// Gates + provisional n + full local build into dist/ — zero commits,
    /// zero network mutations (the one network touch is the gates' fetch).
    pub dry_run: bool,
    /// Re-enter the journaled cut at its first incomplete step.
    pub resume: bool,
    /// Override the version derived from `[workspace.package] version`
    /// (canonical `MAJOR.MINOR.PATCH`, e.g. "0.3.0").
    pub set_version: Option<String>,
    /// Requested operator apply floor / yank. The emitted floor is the maximum
    /// of this value and the newest live channel manifest's carried floor.
    pub min_build: Option<u64>,
    /// Additionally run `tools/verify.sh --full` inline after the gates —
    /// opt-in, never mandatory (spec decisions 15/22).
    pub gate: bool,
    /// "OWNER/REPO": a full real cut published to a scratch repo with a
    /// provisional (never-pushed) ledger number (spec decision 17).
    pub rehearse: Option<String>,
    /// Ship a single-arch build (explicit opt-out of universal, decision 18).
    pub arm64_only: bool,
}

/// Which cut flavor is running — decided once, checked per step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutKind {
    /// The real thing: claim pushed, publish to origin, tag.
    Real,
    /// Stop after the self-check; nothing pushed or uploaded anywhere.
    DryRun,
    /// Publish to the scratch repo; no ledger push, no tag on origin.
    Rehearse,
}

// ---------------------------------------------------------------------------
// transcript printing
// ---------------------------------------------------------------------------

/// One transcript step line: two-space indent, label padded to the 11-column
/// gutter of the spec §5 transcript. Continuation lines pass `""`.
pub fn step(label: &str, msg: &str) {
    println!("  {label:<11}{msg}");
}

/// "4m12s" / "38s" — whole-cut timing for the DONE line.
pub fn fmt_elapsed(start: Instant) -> String {
    let s = start.elapsed().as_secs();
    if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

// ---------------------------------------------------------------------------
// version + slug helpers (pure)
// ---------------------------------------------------------------------------

/// Read the source tree's `[workspace.package]` `MAJOR.MINOR.0` version —
/// the ONE version lineage. A cut never rewrites it; it derives the release
/// version from it (see [`release_version_from_workspace`]).
pub fn workspace_version(cargo_toml: &str) -> Result<String> {
    let mut in_pkg = false;
    for line in cargo_toml.lines() {
        if line.starts_with('[') {
            in_pkg = line.trim() == "[workspace.package]";
            continue;
        }
        if in_pkg && line.trim_start().starts_with("version") {
            let mut parts = line.splitn(3, '"');
            let key = parts.next().unwrap_or("");
            if key.trim_end().strip_suffix('=').map(str::trim_end) == Some("version")
                && let Some(v) = parts.next()
            {
                return Ok(v.to_string());
            }
        }
    }
    Err(Error::new(
        "could not read [workspace.package] version from Cargo.toml".to_string(),
    ))
}

/// Split a canonical three-component version into its numbers. The shape
/// check is [`ledger::check_version_shape`], so every caller gets the same
/// canonical-spelling refusal.
fn version_components(version: &str) -> Result<(u64, u64, u64)> {
    ledger::check_version_shape(version)?;
    let mut parts = version.split('.').map(|p| {
        p.parse::<u64>()
            .map_err(|_| Error::new(format!("version {version:?} has an out-of-range component")))
    });
    let major = parts.next().expect("checked three components")?;
    let minor = parts.next().expect("checked three components")?;
    let patch = parts.next().expect("checked three components")?;
    Ok((major, minor, patch))
}

/// THE cut-over rule: a RELEASE carries the workspace `MAJOR.MINOR.0` version.
/// The patch slot is already `0` under the current scheme, so this is normally
/// the identity — `release_version_from_workspace("0.5.0") == "0.5.0"` — and it
/// additionally normalizes any lingering non-zero patch from the retired
/// `MAJOR.MINOR.DEV` convention (`"0.2.1"` → `"0.2.0"`).
///
/// This is the single source of the version a cut publishes — the ledger is
/// read for the BUILD NUMBER only. To cut again the operator bumps
/// `[workspace.package] version`'s MINOR in Cargo.toml.
pub fn release_version_from_workspace(workspace: &str) -> Result<String> {
    let (major, minor, _dev) = version_components(workspace).map_err(|error| {
        Error::new(format!(
            "Cargo.toml [workspace.package] version is not canonical MAJOR.MINOR.0: {error}"
        ))
    })?;
    Ok(format!("{major}.{minor}.0"))
}

/// The next release version after `release`: bump MINOR, reset the third
/// component to 0. `"0.2.0"` → `"0.3.0"`. Used only to TELL the operator what
/// to bump `[workspace.package] version` to — a cut never applies it.
pub fn bump_minor_release(release: &str) -> Result<String> {
    let (major, minor, _patch) = version_components(release)?;
    let minor = minor.checked_add(1).ok_or_else(|| {
        Error::new(format!(
            "version {release:?} cannot bump MINOR without overflow"
        ))
    })?;
    Ok(format!("{major}.{minor}.0"))
}

/// "owner/repo" from `[workspace.package] repository` — the single source of
/// truth the client's compiled-in default also derives from, so the publish
/// target and the fleet's update source can't drift.
pub fn repo_slug(cargo_toml: &str) -> Option<String> {
    let mut in_pkg = false;
    for line in cargo_toml.lines() {
        if line.starts_with('[') {
            in_pkg = line.trim() == "[workspace.package]";
            continue;
        }
        if in_pkg && line.trim_start().starts_with("repository") {
            let url = line.split('"').nth(1)?;
            let tail = url
                .strip_prefix("https://github.com/")
                .or_else(|| url.strip_prefix("http://github.com/"))
                .or_else(|| url.strip_prefix("git@github.com:"))?;
            let slug = tail.trim_end_matches('/').trim_end_matches(".git");
            if slug.split('/').count() == 2 {
                return Some(slug.to_string());
            }
        }
    }
    None
}

/// The PUBLIC update channel (`OWNER/REPO`) for a checkout, from the tracked
/// `[workspace.metadata.aterm] update_channel`. `Ok(None)` = no public mirror
/// configured. Resume and recovery re-read it from the worktree rather than the
/// journal on purpose: it is tracked repository policy at the claim commit, not
/// per-cut state, and re-reading keeps one answer for the whole pipeline.
fn workspace_mirror_slug(repo: &Path) -> Result<Option<String>> {
    let cargo_text = fs::read_to_string(repo.join("Cargo.toml"))
        .map_err(|error| Error::new(format!("read Cargo.toml: {error}")))?;
    mirror::update_channel_slug(&cargo_text)
}

/// The COMMITTED channel-signing pin for a checkout, from the tracked
/// `[workspace.metadata.aterm] update_channel_pubkey`. `Ok(None)` = no pin,
/// signing stays per-machine opt-in. Re-read from the worktree rather than the
/// journal for the same reason as [`workspace_mirror_slug`]: it is tracked
/// repository policy at the claim commit, and one reader keeps one answer for
/// the whole pipeline (pre-claim, lock, preflip, flip, recovery).
fn workspace_channel_pubkey(_repo: &Path) -> Result<Option<String>> {
    // ONE anchor. This used to parse `[workspace.metadata.aterm]
    // update_channel_pubkey` out of Cargo.toml, which meant the key the CUTTER
    // enforced and the key CLIENTS verify against were two separately edited
    // committed values that nothing compared. Editing one and not the other would
    // have produced releases signed by a key no client accepts — and neither the
    // build nor the cut would have said a word.
    //
    // Both now read `aterm_update_core::pins`. `None` means the channel is
    // unpinned (a fork), exactly as an absent manifest key used to.
    let head = aterm_update_core::pins::update_channel_signing_pubkey();
    Ok((!head.is_empty()).then(|| head.to_string()))
}

/// Parse the GitHub repository addressed by an `origin` URL.  Release state is
/// split between git refs and GitHub Releases, so accepting two independently
/// configured repositories would make every later lease check meaningless.
/// Only unambiguous GitHub HTTPS/SCP/SSH forms are accepted.
pub fn github_slug_from_remote_url(url: &str) -> Result<String> {
    let url = url.trim();
    let tail = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .ok_or_else(|| {
            Error::new(format!(
                "origin URL {url:?} is not an unambiguous GitHub repository URL"
            ))
        })?;
    let slug = tail.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = slug.split('/');
    let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(Error::new(format!(
            "origin URL {url:?} does not name exactly one GitHub OWNER/REPO"
        )));
    };
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    if !valid(owner) || !valid(repo) {
        return Err(Error::new(format!(
            "origin URL {url:?} contains an invalid GitHub OWNER/REPO"
        )));
    }
    Ok(format!("{owner}/{repo}"))
}

/// Bind the git remote used for lease/tag/CAS operations to the Cargo.toml
/// repository used for GitHub release and archive operations.  This runs
/// before every real cut/recovery mutation and is intentionally exact (GitHub
/// may compare names case-insensitively; the release protocol does not).
pub fn assert_origin_repo_binding(git: &dyn GitRunner, expected_slug: &str) -> Result<()> {
    let out = git_ok(git, &["remote", "get-url", "origin"])?;
    let observed = github_slug_from_remote_url(out.stdout_utf8().trim())?;
    if observed != expected_slug {
        return Err(Error::new(format!(
            "release repository split-brain: Cargo.toml names {expected_slug}, but git origin \
             names {observed}; refusing every remote mutation"
        )));
    }
    Ok(())
}

/// One clock reading for the whole cut (retries derive monotonicity from the
/// ledger tail, never from time moving — see ledger::ClaimPlan).
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// gh plumbing (3 retries with backoff — spec §7)
// ---------------------------------------------------------------------------

/// The release-org token, read from disk. Without it `cargo ship cut` authenticates
/// EVERY call with `gh auth token` — the dev account, which has no push on the public
/// update channel, so the mirror step cannot write there and the cut refuses at
/// [`preflight_mirror_target`].
///
/// Same file the publication engine reads (`publication/bin/pub` `MIRROR_TOKEN_PATH`,
/// documented in its `KEYS.md`): one credential for the release org, shared by both
/// pipelines.
fn channel_token_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".secrets/gh_access_token_alabsystems"))
}

/// Read + canonicalize the release-org token. `None` when absent, so a machine
/// without it keeps the previous behaviour (`gh auth`) and simply cannot mirror.
pub(crate) fn channel_token() -> Option<String> {
    let token = fs::read_to_string(channel_token_path()?).ok()?;
    let token = token.trim().to_string();
    (!token.is_empty() && !token.bytes().any(|b| b.is_ascii_control())).then_some(token)
}

/// Is a channel-scoped credential in force for the current operation?
///
/// Set only by [`ChannelCred`], around work that talks EXCLUSIVELY to the public
/// channel. `step_mirror` qualifies: it reads its asset bytes from local `dist/`
/// files and every remote call it makes is against the channel slug, so no private
/// read can be mis-credentialed by the swap. A blanket process-wide swap would NOT
/// be safe for a step that also reads the private release.
static CHANNEL_CRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// RAII scope: channel credential in force until dropped, including on the error
/// paths — a `?` inside the scope must not leave the flag set for later private work.
struct ChannelCred;

impl ChannelCred {
    fn enter() -> Self {
        CHANNEL_CRED.store(true, Ordering::SeqCst);
        Self
    }
}

impl Drop for ChannelCred {
    fn drop(&mut self) {
        CHANNEL_CRED.store(false, Ordering::SeqCst);
    }
}

/// The token to authenticate the current call with, or `None` for `gh`'s own auth.
/// Kept out of argv: callers put it in the environment or a private header file.
fn active_channel_token() -> Option<String> {
    CHANNEL_CRED
        .load(Ordering::SeqCst)
        .then(channel_token)
        .flatten()
}

/// One `gh` invocation, captured. Spawn failure is an error; a non-zero exit
/// is returned to the caller (probes need to see "not found" exits).
pub fn gh_raw(args: &[&str]) -> Result<RunOut> {
    let mut command = Command::new("gh");
    command.args(args);
    // `GH_TOKEN` overrides `gh auth` for this child only — never a global env
    // mutation, so a concurrent private-repo call is unaffected.
    if let Some(token) = active_channel_token() {
        command.env("GH_TOKEN", token);
    }
    let out = command
        .output()
        .map_err(|e| Error::new(format!("failed to spawn gh {}: {e}", args.join(" "))))?;
    Ok(RunOut {
        status: out.status.code().unwrap_or(-1),
        stdout: out.stdout,
        stderr: out.stderr,
    })
}

/// `gh` with success REQUIRED, retried 3 times with backoff (2s, 5s) — the
/// GitHub API flakes; a mid-cut transient must not wedge a ten-minute build.
/// Every operation retried THROUGH HERE is idempotent (metadata edits and
/// guarded deletes converge). Draft creation and asset upload are NOT:
/// GitHub may accept either POST while its response is lost. Those operations
/// persist one-shot intents and never pass through this retry helper.
pub fn gh_retry(args: &[&str]) -> Result<RunOut> {
    gh_retry_guarded(args, || Ok(()))
}

/// Mutation retry seam: revalidate the exact process fence immediately before
/// EVERY attempt, including retries after a timeout/backoff.  A one-time step
/// entry check would let a rotated stale process wake and mutate later.
fn gh_retry_guarded(
    args: &[&str],
    mut before_each_attempt: impl FnMut() -> Result<()>,
) -> Result<RunOut> {
    let mut last = String::new();
    for (attempt, backoff) in [(1u32, 2u64), (2, 5), (3, 0)] {
        before_each_attempt()?;
        let out = gh_raw(args)?;
        if out.success() {
            return Ok(out);
        }
        last = out.stderr_utf8().trim().to_string();
        if attempt < 3 {
            eprintln!(
                "    gh {} failed (attempt {attempt}/3): {last} — retrying in {backoff}s",
                args.first().unwrap_or(&"")
            );
            std::thread::sleep(std::time::Duration::from_secs(backoff));
        }
    }
    Err(Error::new(format!(
        "gh {} failed after 3 attempts: {last}",
        args.join(" ")
    )))
}

// ---------------------------------------------------------------------------
// cross-machine release lease (atomic remote lightweight tag)
// ---------------------------------------------------------------------------

/// Dedicated cooperative lock for every REAL release cut. A lightweight tag
/// points at the journaled claim commit, making ownership inspectable and
/// recoverable on another machine without a mutable lock payload.
pub const RELEASE_LEASE_REF: &str = "refs/tags/aterm-release-lease";

/// Per-invocation fencing token.  The persistent lease deliberately points at
/// the claim commit so any machine can identify the cut; that identity is not
/// unique between two simultaneous resumes.  This second ref points at a
/// unique annotated-tag object which peels to the same claim, giving each
/// publisher process an exact compare-and-swap token.
pub const PUBLISHER_FENCE_REF: &str = "refs/tags/aterm-release-fence";

/// Mandatory acknowledgement for the one recovery operation whose safety has
/// an external, operator-established precondition. This is deliberately an
/// assertion, not a claim that the program can prove process quiescence.
pub const RECOVERY_STOPPED_PROCESS_FLAG: &str = "--old-publisher-stopped";
pub const RECOVERY_STOPPED_PROCESS_REFUSAL: &str = "lost-machine recovery requires explicit proof that the old publisher process is stopped; \
     a fence rotation cannot cancel an already in-flight GitHub REST request";
pub const RECOVERY_STOPPED_PROCESS_BANNER: &str =
    "OPERATOR ASSERTION: old publisher is stopped; Git fencing cannot cancel in-flight REST";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseLeaseGuard {
    owner: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublisherFenceGuard {
    owner: String,
    token: String,
}

impl PublisherFenceGuard {
    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}

/// Authoritative remote fence state: `token` is the annotated-tag object and
/// `owner` is its peeled claim commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublisherFence {
    pub token: String,
    pub owner: String,
}

impl ReleaseLeaseGuard {
    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn is_owner(&self, observed: Option<&str>) -> bool {
        observed == Some(self.owner.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseAcquireAction {
    Create,
    AlreadyOwned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseRelease {
    Released,
    AlreadyAbsent,
    /// Our completed cut's delete landed, then a successor acquired the ref.
    /// The foreign owner is observed and deliberately left untouched.
    AlreadySuperseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceRelease {
    Released,
    AlreadyAbsent,
    /// Our exact token disappeared and a new session won create/rotation.
    AlreadySuperseded,
}

fn valid_lease_owner(owner: &str) -> bool {
    matches!(owner.len(), 40 | 64) && owner.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Pure `AcquireLease` decision seam used by production and Tier-1 bindings.
/// A different owner is terminal: no force update or stealing is ever offered.
pub fn acquire_lease_action(
    observed: Option<&str>,
    expected_owner: &str,
) -> Result<LeaseAcquireAction> {
    if !valid_lease_owner(expected_owner) {
        return Err(Error::new(format!(
            "release lease owner {expected_owner:?} is not a full git object id"
        )));
    }
    let expected_owner = expected_owner.to_ascii_lowercase();
    match observed.map(str::to_ascii_lowercase).as_deref() {
        None => Ok(LeaseAcquireAction::Create),
        Some(owner) if owner == expected_owner => Ok(LeaseAcquireAction::AlreadyOwned),
        Some(owner) => Err(Error::new(format!(
            "release lease {RELEASE_LEASE_REF} is owned by {owner}, not {expected_owner}; \
             refusing to steal or force-update it"
        ))),
    }
}

/// Read the exact lightweight lock ref. Multiple/malformed answers fail closed.
pub fn release_lease_owner(git: &dyn GitRunner) -> Result<Option<String>> {
    let out = git_ok(git, &["ls-remote", "origin", RELEASE_LEASE_REF])?;
    let text = out.stdout_utf8();
    let rows: Vec<&str> = text.lines().collect();
    if rows.is_empty() {
        return Ok(None);
    }
    if rows.len() != 1 {
        return Err(Error::new(format!(
            "release lease query returned {} rows for {RELEASE_LEASE_REF}",
            rows.len()
        )));
    }
    let mut fields = rows[0].split_whitespace();
    let (Some(owner), Some(reference), None) = (fields.next(), fields.next(), fields.next()) else {
        return Err(Error::new("malformed release lease ls-remote response"));
    };
    if reference != RELEASE_LEASE_REF || !valid_lease_owner(owner) {
        return Err(Error::new(format!(
            "malformed release lease row: {:?}",
            rows[0]
        )));
    }
    Ok(Some(owner.to_ascii_lowercase()))
}

/// Read the unique annotated publisher fence and its peeled claim.  A
/// lightweight ref, a missing peel, extra rows, or malformed object ids all
/// fail closed: such a ref cannot prove either session identity or ownership.
pub fn publisher_fence(git: &dyn GitRunner) -> Result<Option<PublisherFence>> {
    let peeled_ref = format!("{PUBLISHER_FENCE_REF}^{{}}");
    let out = git_ok(
        git,
        &["ls-remote", "origin", PUBLISHER_FENCE_REF, &peeled_ref],
    )?;
    let text = out.stdout_utf8();
    let rows: Vec<&str> = text.lines().collect();
    if rows.is_empty() {
        return Ok(None);
    }
    if rows.len() != 2 {
        return Err(Error::new(format!(
            "publisher fence query returned {} rows; expected an annotated ref plus peel",
            rows.len()
        )));
    }
    let mut token = None;
    let mut owner = None;
    for row in rows {
        let mut fields = row.split_whitespace();
        let (Some(oid), Some(reference), None) = (fields.next(), fields.next(), fields.next())
        else {
            return Err(Error::new("malformed publisher fence ls-remote response"));
        };
        if !valid_lease_owner(oid) {
            return Err(Error::new(format!(
                "publisher fence contains malformed object id {oid:?}"
            )));
        }
        match reference {
            PUBLISHER_FENCE_REF => token = Some(oid.to_ascii_lowercase()),
            reference if reference == peeled_ref => owner = Some(oid.to_ascii_lowercase()),
            _ => {
                return Err(Error::new(format!(
                    "publisher fence query returned unexpected ref {reference:?}"
                )));
            }
        }
    }
    let (Some(token), Some(owner)) = (token, owner) else {
        return Err(Error::new(
            "publisher fence is not an annotated tag peeled to a claim commit",
        ));
    };
    if token == owner {
        return Err(Error::new(
            "publisher fence token equals its owner; refusing a lightweight/non-unique fence",
        ));
    }
    Ok(Some(PublisherFence { token, owner }))
}

fn ensure_no_publisher_fence(git: &dyn GitRunner) -> Result<()> {
    if let Some(fence) = publisher_fence(git)? {
        return Err(Error::new(format!(
            "publisher fence {PUBLISHER_FENCE_REF} is active at token {} for claim {}; \
             another publisher or a killed process may still be in flight. Do not steal it: \
             resume after that process exits, or run `cargo ship recover vX.Y.Z <full-claim-sha>` \
             with `--old-publisher-stopped` only after proving the old process is stopped",
            fence.token, fence.owner
        )));
    }
    Ok(())
}

/// Read-only fresh-cut preflight. It reports an existing owner before a ledger
/// claim can be burned; the later atomic create still closes the check race.
pub fn preflight_release_lease(git: &dyn GitRunner) -> Result<()> {
    ensure_no_publisher_fence(git)?;
    if let Some(owner) = release_lease_owner(git)? {
        return Err(Error::new(format!(
            "release lease {RELEASE_LEASE_REF} is already owned by {owner}; resume/abandon \
             that exact journal before claiming another build"
        )));
    }
    Ok(())
}

/// Production `AcquireLease`: create-only push, followed by an authoritative
/// owner read. An existing exact owner is resume; a competing owner is refusal.
pub fn acquire_release_lease(
    git: &dyn GitRunner,
    expected_owner: &str,
) -> Result<ReleaseLeaseGuard> {
    // This owner operation is used by cut and abandon.  Neither may enter
    // while a prior process fence survives; only the explicit recovery lane
    // has authority to rotate an observed exact token.
    ensure_no_publisher_fence(git)?;
    let expected_owner = expected_owner.to_ascii_lowercase();
    let observed = release_lease_owner(git)?;
    if acquire_lease_action(observed.as_deref(), &expected_owner)? == LeaseAcquireAction::Create {
        let spec = format!("{expected_owner}:{RELEASE_LEASE_REF}");
        let pushed = git.git(&["push", "origin", &spec])?;
        if !pushed.success() {
            let now = release_lease_owner(git)?;
            if now.as_deref() != Some(expected_owner.as_str()) {
                return Err(Error::new(format!(
                    "atomic release lease create lost: {}; owner is {}",
                    pushed.stderr_utf8().trim(),
                    now.as_deref().unwrap_or("absent")
                )));
            }
        }
    }
    let owner = release_lease_owner(git)?;
    acquire_lease_action(owner.as_deref(), &expected_owner)?;
    Ok(ReleaseLeaseGuard {
        owner: expected_owner,
    })
}

fn new_publisher_fence_token(git: &dyn GitRunner, owner: &str) -> Result<String> {
    static FENCE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    if !valid_lease_owner(owner) {
        return Err(Error::new("cannot fence a malformed claim object id"));
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = FENCE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let local = format!(
        "aterm-release-fence-candidate-{}-{nonce}-{sequence}",
        std::process::id(),
    );
    let message = format!(
        "aterm publisher fence for claim {owner}; pid {}; nonce {nonce}; sequence {sequence}",
        std::process::id()
    );
    git_ok(git, &["tag", "-a", &local, "-m", &message, owner])?;
    let token_result = (|| {
        let out = git_ok(git, &["rev-parse", &format!("refs/tags/{local}")])?;
        let token = out.stdout_utf8().trim().to_ascii_lowercase();
        if !valid_lease_owner(&token) || token == owner {
            return Err(Error::new(
                "git did not create a unique annotated publisher-fence object",
            ));
        }
        Ok(token)
    })();
    // The candidate ref is process-local scaffolding only.  Its object remains
    // available for the subsequent push after the ref is removed.
    let cleanup = git_ok(git, &["tag", "-d", &local]).map(|_| ());
    match (token_result, cleanup) {
        (Ok(token), Ok(())) => Ok(token),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(Error::new(format!(
            "created a publisher fence candidate but could not remove its local ref: {error}"
        ))),
    }
}

fn confirm_release_lease_owner(
    git: &dyn GitRunner,
    expected_owner: &str,
) -> Result<ReleaseLeaseGuard> {
    let expected_owner = expected_owner.to_ascii_lowercase();
    let observed = release_lease_owner(git)?;
    if observed.as_deref() != Some(expected_owner.as_str()) {
        return Err(Error::new(format!(
            "release lease ownership changed: expected {expected_owner}, observed {}",
            observed.as_deref().unwrap_or("absent")
        )));
    }
    Ok(ReleaseLeaseGuard {
        owner: expected_owner,
    })
}

/// Create a unique per-process fence.  Even resumes carrying the same claim
/// owner race through a create-only push, so at most one can mutate GitHub.
pub fn acquire_publisher_fence(
    git: &dyn GitRunner,
    expected_owner: &str,
) -> Result<PublisherFenceGuard> {
    confirm_release_lease_owner(git, expected_owner)?;
    ensure_no_publisher_fence(git)?;
    let token = new_publisher_fence_token(git, expected_owner)?;
    let spec = format!("{token}:{PUBLISHER_FENCE_REF}");
    let pushed = git.git(&["push", "origin", &spec])?;
    let now = publisher_fence(git)?;
    if now.as_ref().is_some_and(|fence| {
        fence.token == token && fence.owner.eq_ignore_ascii_case(expected_owner)
    }) {
        return Ok(PublisherFenceGuard {
            owner: expected_owner.to_ascii_lowercase(),
            token,
        });
    }
    Err(Error::new(format!(
        "atomic publisher-fence create lost: {}; current token is {}",
        pushed.stderr_utf8().trim(),
        now.as_ref().map_or("absent", |fence| fence.token.as_str())
    )))
}

/// Explicit killed-machine takeover.  The caller supplies and validates the
/// claim identity first; this function atomically replaces only the exact
/// observed stale token.  Two recovery commands racing from the same
/// observation have one winner, and no time-based/automatic stealing exists.
pub fn rotate_publisher_fence_for_recovery(
    git: &dyn GitRunner,
    expected_owner: &str,
) -> Result<PublisherFenceGuard> {
    confirm_release_lease_owner(git, expected_owner)?;
    let observed = publisher_fence(git)?;
    if let Some(fence) = &observed
        && !fence.owner.eq_ignore_ascii_case(expected_owner)
    {
        return Err(Error::new(format!(
            "publisher fence peels to {}, not recovery claim {expected_owner}; refusing takeover",
            fence.owner
        )));
    }
    let token = new_publisher_fence_token(git, expected_owner)?;
    if observed.as_ref().is_some_and(|fence| fence.token == token) {
        return Err(Error::new(
            "publisher-fence rotation generated the old token again; refusing a non-fencing takeover",
        ));
    }
    let spec = format!("{token}:{PUBLISHER_FENCE_REF}");
    let out = if let Some(fence) = &observed {
        let lease = format!("--force-with-lease={PUBLISHER_FENCE_REF}:{}", fence.token);
        git.git(&["push", &lease, "origin", &spec])?
    } else {
        git.git(&["push", "origin", &spec])?
    };
    let now = publisher_fence(git)?;
    if now.as_ref().is_some_and(|fence| {
        fence.token == token && fence.owner.eq_ignore_ascii_case(expected_owner)
    }) {
        return Ok(PublisherFenceGuard {
            owner: expected_owner.to_ascii_lowercase(),
            token,
        });
    }
    Err(Error::new(format!(
        "publisher-fence recovery rotation lost its exact CAS: {}; current token is {}",
        out.stderr_utf8().trim(),
        now.as_ref().map_or("absent", |fence| fence.token.as_str())
    )))
}

/// Re-prove both persistent claim ownership and the exact process token before
/// each visibility/archive mutation.
pub fn assert_publisher_session(
    git: &dyn GitRunner,
    lease: &ReleaseLeaseGuard,
    fence: &PublisherFenceGuard,
) -> Result<()> {
    let owner = release_lease_owner(git)?;
    if !lease.is_owner(owner.as_deref()) || fence.owner() != lease.owner {
        return Err(Error::new(format!(
            "publisher session lost claim ownership: expected {}, observed {}",
            lease.owner,
            owner.as_deref().unwrap_or("absent")
        )));
    }
    let observed = publisher_fence(git)?;
    if observed
        .as_ref()
        .is_none_or(|current| current.token != fence.token() || current.owner != fence.owner())
    {
        return Err(Error::new(format!(
            "publisher session was fenced out: expected token {}, observed {}",
            fence.token(),
            observed
                .as_ref()
                .map_or("absent", |current| current.token.as_str())
        )));
    }
    Ok(())
}

/// Delete only this process's exact token.  A different token is a successor
/// session and is left byte-for-byte untouched.
pub fn release_publisher_fence(
    git: &dyn GitRunner,
    guard: &PublisherFenceGuard,
) -> Result<FenceRelease> {
    match publisher_fence(git)? {
        None => return Ok(FenceRelease::AlreadyAbsent),
        Some(current) if current.token != guard.token => {
            return Ok(FenceRelease::AlreadySuperseded);
        }
        Some(current) if current.owner != guard.owner => {
            return Err(Error::new(format!(
                "publisher fence token {} unexpectedly peels to {}, not {}; refusing delete",
                current.token, current.owner, guard.owner
            )));
        }
        Some(_) => {}
    }
    let lease = format!("--force-with-lease={PUBLISHER_FENCE_REF}:{}", guard.token);
    let delete = format!(":{PUBLISHER_FENCE_REF}");
    let out = git.git(&["push", &lease, "origin", &delete])?;
    match publisher_fence(git)? {
        None => Ok(FenceRelease::Released),
        Some(current) if current.token != guard.token => Ok(FenceRelease::AlreadySuperseded),
        Some(_) => Err(Error::new(format!(
            "CAS release of publisher fence failed: {}",
            out.stderr_utf8().trim()
        ))),
    }
}

/// Final unlock deletes the persistent owner and the process token in ONE
/// atomic ref transaction.  Deleting the owner first could strand a killed
/// process's fence with no claim identity; deleting the fence first could let
/// a same-claim resume enter while the old process was still unlocking.
pub fn release_completed_publisher_session(
    git: &dyn GitRunner,
    expected_owner: &str,
    guard: &PublisherFenceGuard,
) -> Result<LeaseRelease> {
    let expected_owner = expected_owner.to_ascii_lowercase();
    let owner = release_lease_owner(git)?;
    let fence = publisher_fence(git)?;
    match (owner.as_deref(), fence.as_ref()) {
        (None, None) => return Ok(LeaseRelease::AlreadyAbsent),
        (Some(observed), Some(current))
            if observed == expected_owner
                && current.token == guard.token()
                && current.owner == expected_owner => {}
        (Some(observed), Some(current))
            if current.token != guard.token() && current.owner == observed =>
        {
            return Ok(LeaseRelease::AlreadySuperseded);
        }
        _ => {
            return Err(Error::new(format!(
                "refusing non-atomic/inconsistent final unlock: owner {}, fence token {}",
                owner.as_deref().unwrap_or("absent"),
                fence
                    .as_ref()
                    .map_or("absent", |current| current.token.as_str())
            )));
        }
    }
    let owner_lease = format!("--force-with-lease={RELEASE_LEASE_REF}:{expected_owner}");
    let fence_lease = format!("--force-with-lease={PUBLISHER_FENCE_REF}:{}", guard.token());
    let owner_delete = format!(":{RELEASE_LEASE_REF}");
    let fence_delete = format!(":{PUBLISHER_FENCE_REF}");
    let out = git.git(&[
        "push",
        "--atomic",
        &owner_lease,
        &fence_lease,
        "origin",
        &owner_delete,
        &fence_delete,
    ])?;
    let owner_now = release_lease_owner(git)?;
    let fence_now = publisher_fence(git)?;
    match (owner_now.as_deref(), fence_now.as_ref()) {
        (None, None) => Ok(LeaseRelease::Released),
        // Our atomic delete may have landed and a successor may have completed
        // only the create-only owner half of acquisition before this read.  An
        // owner can reappear only after both of our refs were atomically absent;
        // never touch that successor, even while its fence creation is in flight.
        (Some(_), None) => Ok(LeaseRelease::AlreadySuperseded),
        (Some(owner), Some(current))
            if (owner != expected_owner || current.token != guard.token())
                && current.owner == owner =>
        {
            Ok(LeaseRelease::AlreadySuperseded)
        }
        _ => Err(Error::new(format!(
            "atomic final unlock failed or left inconsistent refs: {}; owner {}, fence {}",
            out.stderr_utf8().trim(),
            owner_now.as_deref().unwrap_or("absent"),
            fence_now
                .as_ref()
                .map_or("absent", |current| current.token.as_str())
        ))),
    }
}

/// Pure `PublishChecked` seam: the same owner guard must still cover the late
/// channel verdict. This is called at every real visibility/check boundary.
pub fn publish_checked(
    guard: &ReleaseLeaseGuard,
    observed_owner: Option<&str>,
    carried_floor: Option<u64>,
    newest_floor: Option<u64>,
) -> Result<()> {
    if !guard.is_owner(observed_owner) {
        return Err(Error::new(format!(
            "release lease ownership changed before PublishChecked: expected {}, observed {}",
            guard.owner(),
            observed_owner.unwrap_or("absent")
        )));
    }
    channel_floor_covered(carried_floor, newest_floor)
}

/// CAS-safe unlock. Deletion is permitted only with the exact expected owner;
/// an already-absent ref converges a crash after delete/before journal mark.
#[allow(dead_code)] // exercised by integration/Tier-1 fixtures; production uses the paired unlock
pub fn release_release_lease(git: &dyn GitRunner, expected_owner: &str) -> Result<LeaseRelease> {
    release_release_lease_inner(git, expected_owner, false)
}

/// Unlock-only crash convergence. This is valid exclusively after every
/// publishing step is journaled: a foreign create-only owner proves our ref
/// was absent after our prior CAS delete, so it is a successor, not a lease
/// we may touch. All earlier states use [`release_release_lease`] and refuse.
pub fn release_completed_release_lease(
    git: &dyn GitRunner,
    expected_owner: &str,
) -> Result<LeaseRelease> {
    release_release_lease_inner(git, expected_owner, true)
}

/// Unlock-only replay when this process has no fence guard (the crash may have
/// happened after the atomic owner+fence delete but before the journal mark).
/// A coherent foreign pair is a proven successor and remains untouched;
/// same-owner or incoherent surviving tokens require explicit recovery.
pub fn release_completed_session_without_guard(
    git: &dyn GitRunner,
    expected_owner: &str,
) -> Result<LeaseRelease> {
    let expected_owner = expected_owner.to_ascii_lowercase();
    let owner = release_lease_owner(git)?;
    let observed_fence = publisher_fence(git)?;
    match (owner.as_deref(), observed_fence.as_ref()) {
        (Some(current_owner), Some(current_fence))
            if current_owner != expected_owner && current_fence.owner == current_owner =>
        {
            Ok(LeaseRelease::AlreadySuperseded)
        }
        (Some(current_owner), Some(stale))
            if current_owner == expected_owner && stale.owner == current_owner =>
        {
            Err(Error::new(format!(
                "unlock-only resume found killed publisher token {} for claim {expected_owner}; \
                 explicit recovery must rotate it",
                stale.token
            )))
        }
        (_, Some(stale)) => Err(Error::new(format!(
            "unlock-only resume found incoherent publisher refs: owner {}, fence token {} peels \
             to {}; refusing to delete either ref",
            owner.as_deref().unwrap_or("absent"),
            stale.token,
            stale.owner
        ))),
        (_, None) => release_completed_release_lease(git, &expected_owner),
    }
}

fn release_release_lease_inner(
    git: &dyn GitRunner,
    expected_owner: &str,
    allow_successor: bool,
) -> Result<LeaseRelease> {
    let expected_owner = expected_owner.to_ascii_lowercase();
    match release_lease_owner(git)? {
        None => return Ok(LeaseRelease::AlreadyAbsent),
        Some(owner) if owner != expected_owner && allow_successor => {
            return Ok(LeaseRelease::AlreadySuperseded);
        }
        Some(owner) if owner != expected_owner => {
            return Err(Error::new(format!(
                "release lease is owned by {owner}, not {expected_owner}; refusing to delete \
                 another cut's lease"
            )));
        }
        Some(_) => {}
    }
    let lease = format!("--force-with-lease={RELEASE_LEASE_REF}:{expected_owner}");
    let delete = format!(":{RELEASE_LEASE_REF}");
    let out = git.git(&["push", &lease, "origin", &delete])?;
    let now = release_lease_owner(git)?;
    if now.is_none() {
        return Ok(LeaseRelease::Released);
    }
    // We observed our exact owner immediately before the CAS attempt. Any
    // different create-only owner observed now can exist only after ours was
    // absent, regardless of whether the transport reported success.
    if now.as_deref() != Some(expected_owner.as_str()) {
        return Ok(LeaseRelease::AlreadySuperseded);
    }
    Err(Error::new(format!(
        "CAS release of {RELEASE_LEASE_REF} failed: {}; current owner is {}",
        out.stderr_utf8().trim(),
        now.as_deref().unwrap_or("absent")
    )))
}

// ---------------------------------------------------------------------------
// the resume journal (dist/cut-state.toml)
// ---------------------------------------------------------------------------

/// Pipeline steps in execution order, as journaled. Gates + claim precede the
/// journal's existence (a journal on disk MEANS the claim is verified);
/// "build" covers build+bundle+sign+dmg+manifest as one re-enterable unit
/// (its outputs are all derived from `(version, build_number)` on disk).
pub const STEPS: [&str; 12] = [
    "lock",
    "build",
    "selfcheck",
    "draft",
    "upload",
    "preflip",
    "tag",
    "flip",
    "archive",
    "verify",
    "mirror",
    "unlock",
];

const LEGACY_STEPS: [&str; 9] = [
    "build",
    "selfcheck",
    "draft",
    "upload",
    "preflip",
    "tag",
    "flip",
    "cask",
    "verify",
];

/// Format-5 step order — identical to [`STEPS`] minus the public-channel
/// `mirror` step, which format 6 inserted between `verify` and `unlock`. A
/// COMPLETED v5 journal must still read back as complete (it is history a
/// `status`/fresh cut clears); walking it against the current list would report
/// the mirror as its next step and misfile a finished cut as resumable.
const STEPS_V5: [&str; 12] = [
    "lock",
    "build",
    "selfcheck",
    "draft",
    "upload",
    "preflip",
    "tag",
    "flip",
    "archive",
    "cask",
    "verify",
    "unlock",
];

/// Format-6 step order — identical to [`STEPS`] plus the retired Homebrew
/// `cask` step, which format 7 removed from between `archive` and `verify`.
/// Frozen for the same reason as [`STEPS_V5`]: a COMPLETED v6 journal must
/// still read back as complete. Walking one against the current list is
/// harmless (a removed step can only make an old journal look *more*
/// complete), but walking an UNFINISHED v6 journal against it would skip the
/// cask entry it legitimately still owes, so the historical list stays.
const STEPS_V6: [&str; 13] = [
    "lock",
    "build",
    "selfcheck",
    "draft",
    "upload",
    "preflip",
    "tag",
    "flip",
    "archive",
    "cask",
    "verify",
    "mirror",
    "unlock",
];

pub const JOURNAL_FORMAT: u32 = 7;

const fn legacy_journal_format() -> u32 {
    1
}

/// The cut journal — everything a re-entry (this machine or, together with
/// the remote-derived recut, any machine) needs to finish or abandon a cut.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    /// Recovery protocol. Missing means the pre-lease/pre-archive v1 format.
    #[serde(default = "legacy_journal_format")]
    pub format: u32,
    /// Release version being cut, canonical `MAJOR.MINOR.PATCH` ("0.2.0").
    pub version: String,
    /// The verified ledger claim n.
    pub build_number: u64,
    /// The claim commit (full sha) — artifacts must come from exactly here.
    pub commit: String,
    /// Effective channel floor frozen at claim time: max(operator request,
    /// newest live manifest floor). Resume must rebuild the same manifest.
    #[serde(default)]
    pub min_build: Option<u64>,
    #[serde(default)]
    pub arm64_only: bool,
    /// Whether this cut's uploaded channel manifest has a detached signature.
    /// Persisted so archive resume enforces the same paired-head invariant.
    #[serde(default)]
    pub manifest_signed: bool,
    /// Monotonic channel policy derived before the ledger claim.  Once any
    /// exact or archived historical signature exists this can never return to
    /// false, even if the current exact asset name was migrated.
    #[serde(default)]
    pub signature_required: bool,
    /// The canonical base64 Ed25519 public key actually derived from the
    /// owner signing key and proven against signed channel history.  Public by
    /// definition; the private key is never journaled or printed.
    #[serde(default)]
    pub signature_pubkey: Option<String>,
    /// Immutable GitHub release object capability. Draft tag names are not
    /// unique, so every upload/edit/flip/delete after `draft` is pinned to
    /// this ID and revalidates its tag, target commit, and draft state.
    #[serde(default)]
    pub release_id: Option<u64>,
    /// Durable one-shot create intent. Set and fsync/rename-persisted before
    /// the non-idempotent draft POST; if the response/object visibility is
    /// ambiguous, resume may discover the object but may never POST again.
    #[serde(default)]
    pub draft_create_issued: bool,
    /// Exact asset names for which an upload POST has ever been issued. The
    /// set is append-only: an absent name after an ambiguous response may be
    /// eventual consistency, so resume must discover it rather than POSTing a
    /// duplicate object.
    #[serde(default)]
    pub upload_intents: Vec<String>,
    /// Immutable GitHub release object capability on the PUBLIC update channel
    /// (`[workspace.metadata.aterm] update_channel`). The mirror is a second
    /// repository with its own object identity, so it gets its own capability
    /// rather than reusing [`Journal::release_id`].
    #[serde(default)]
    pub mirror_release_id: Option<u64>,
    /// Durable one-shot create intent for the mirrored draft. Same contract as
    /// [`Journal::draft_create_issued`]: once persisted, an invisible object
    /// means "discover it", never "POST again" — a duplicate draft on the
    /// public channel would be ambiguous authority in front of the whole fleet.
    #[serde(default)]
    pub mirror_create_issued: bool,
    /// Exact asset names for which a mirror upload POST has ever been issued.
    /// Append-only, exactly like [`Journal::upload_intents`].
    #[serde(default)]
    pub mirror_upload_intents: Vec<String>,
    /// Completed steps, in completion order (a subset of [`STEPS`]).
    #[serde(default)]
    pub done: Vec<String>,
}

impl Journal {
    /// Read the journal; `Ok(None)` when absent. Unparseable is an ERROR (a
    /// half-written journal must stop resume, not silently restart a cut).
    pub fn load(path: &Path) -> Result<Option<Journal>> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::new(format!("read {}: {e}", path.display()))),
        };
        let journal: Journal = toml::from_str(&text)
            .map_err(|e| Error::new(format!("parse {}: {e}", path.display())))?;
        journal.validate()?;
        Ok(Some(journal))
    }

    /// Persist atomically (temp + rename): a crash mid-write must never leave
    /// a torn journal that blocks its own recovery path.
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let text =
            toml::to_string(self).map_err(|e| Error::new(format!("serialize journal: {e}")))?;
        // The journal's directory (dist/, git-ignored) may not exist yet: the
        // FIRST save happens the moment the claim is verified — before the
        // build step's create_dir_all ever runs — and a fresh clone (the spec
        // §5 cross-machine recut state) has no dist/ at all. Failing here
        // would burn the just-pushed ledger number with nothing built, and
        // every retry would recut and burn another.
        let mut newly_created_dirs = Vec::new();
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            let mut cursor = dir;
            while !cursor.exists() {
                newly_created_dirs.push(cursor.to_path_buf());
                cursor = cursor.parent().ok_or_else(|| {
                    Error::new(format!(
                        "journal parent {} has no existing ancestor",
                        dir.display()
                    ))
                })?;
            }
            fs::create_dir_all(dir)
                .map_err(|e| Error::new(format!("create {}: {e}", dir.display())))?;
        }
        let tmp = path.with_extension(format!(
            "toml.{}.{}.tmp",
            std::process::id(),
            RELEASE_ASSET_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)
            .map_err(|e| Error::new(format!("create {}: {e}", tmp.display())))?;
        file.write_all(text.as_bytes())
            .map_err(|e| Error::new(format!("write {}: {e}", tmp.display())))?;
        file.sync_all()
            .map_err(|e| Error::new(format!("fsync {}: {e}", tmp.display())))?;
        drop(file);
        fs::rename(&tmp, path).map_err(|e| {
            Error::new(format!(
                "rename {} -> {}: {e}",
                tmp.display(),
                path.display()
            ))
        })?;
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|e| {
                    Error::new(format!(
                        "fsync journal parent directory {}: {e}",
                        parent.display()
                    ))
                })?;
        }
        // If this was the first journal write in a fresh clone, syncing dist/
        // is insufficient: its own directory entry also has to survive in the
        // repository directory. For a deeper caller-supplied path, sync every
        // newly-created directory's parent up to the first pre-existing one.
        for created in newly_created_dirs {
            let parent = created.parent().ok_or_else(|| {
                Error::new(format!(
                    "new journal directory {} has no parent to fsync",
                    created.display()
                ))
            })?;
            fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|e| {
                    Error::new(format!(
                        "fsync newly-created journal directory parent {}: {e}",
                        parent.display()
                    ))
                })?;
        }
        Ok(())
    }

    pub fn is_done(&self, step: &str) -> bool {
        self.done.iter().any(|s| s == step)
    }

    /// A corrupt/stale journal must not become an authority for an impossible
    /// manifest during resume. New journals are canonicalized by
    /// [`effective_min_build`]; this also protects journals written by older
    /// binaries or edited by hand.
    fn validate(&self) -> Result<()> {
        if !(1..=JOURNAL_FORMAT).contains(&self.format) {
            return Err(Error::new(format!(
                "unsupported release journal format {} (this cutter accepts completed formats 1–{}, refuses unfinished legacy formats, and writes {})",
                self.format,
                JOURNAL_FORMAT - 1,
                JOURNAL_FORMAT
            )));
        }
        if self.format == JOURNAL_FORMAT {
            ledger::check_version_shape(&self.version).map_err(|error| {
                Error::new(format!(
                    "current release journal has invalid version: {error}"
                ))
            })?;
            if !valid_lease_owner(&self.commit) {
                return Err(Error::new(
                    "current release journal commit is not a full 40- or 64-hex claim object id",
                ));
            }
            if self.done.len() > STEPS.len()
                || self
                    .done
                    .iter()
                    .zip(STEPS)
                    .any(|(observed, expected)| observed != expected)
            {
                return Err(Error::new(
                    "current release journal done list is not an exact known, unique, ordered, \
                     gap-free prefix of the canonical pipeline",
                ));
            }
        }
        validate_min_build(self.min_build, self.build_number, "journaled build")?;
        if self.signature_required {
            let pubkey = self.signature_pubkey.as_deref().ok_or_else(|| {
                Error::new("signed release journal has no persisted update public key")
            })?;
            canonical_update_pubkey(pubkey)?;
        } else if self.signature_pubkey.is_some() || self.manifest_signed {
            return Err(Error::new(
                "release journal carries signature bytes/key while signature_required is false",
            ));
        }
        if self.is_done("build") && self.signature_required && !self.manifest_signed {
            return Err(Error::new(
                "release journal marks build complete without its required manifest signature",
            ));
        }
        if self.format == JOURNAL_FORMAT {
            if self.is_done("draft") && self.release_id.is_none_or(|id| id == 0) {
                return Err(Error::new(
                    "current release journal marks draft complete without a nonzero immutable GitHub release ID",
                ));
            }
            if self.is_done("draft") && !self.draft_create_issued {
                return Err(Error::new(
                    "current release journal marks draft complete without durable create intent",
                ));
            }
            if self.release_id.is_some() && !self.draft_create_issued {
                return Err(Error::new(
                    "release journal carries an immutable release ID without durable create intent",
                ));
            }
            Self::validate_upload_intent_set(
                "",
                self.release_id,
                self.draft_create_issued,
                &self.upload_intents,
            )?;
            // The public-channel mirror enforces the private side's capability
            // invariants: an object ID implies a durable create intent, and
            // upload intents imply both. A journal that failed these could
            // authorize a second POST against the channel the whole fleet reads.
            if self.mirror_release_id.is_some_and(|id| id == 0) {
                return Err(Error::new(
                    "release journal carries a zero mirror release ID",
                ));
            }
            if self.mirror_release_id.is_some() && !self.mirror_create_issued {
                return Err(Error::new(
                    "release journal carries a mirror release ID without durable create intent",
                ));
            }
            Self::validate_upload_intent_set(
                "mirror ",
                self.mirror_release_id,
                self.mirror_create_issued,
                &self.mirror_upload_intents,
            )?;
        }
        Ok(())
    }

    /// The shared private/mirror upload-intent invariants: every intent name is
    /// non-empty, in the exact upload URL alphabet, and unique; any intent at
    /// all implies the durable draft capability (a persisted release ID and
    /// create intent). `label` is `""` for the private side, `"mirror "` for
    /// the channel side.
    fn validate_upload_intent_set(
        label: &str,
        release_id: Option<u64>,
        create_issued: bool,
        upload_intents: &[String],
    ) -> Result<()> {
        let mut intents = std::collections::BTreeSet::new();
        if upload_intents.iter().any(|name| {
            name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
                || !intents.insert(name)
        }) {
            return Err(Error::new(format!(
                "release journal {label}upload intents are empty, non-canonical, or duplicated"
            )));
        }
        if !upload_intents.is_empty() && (release_id.is_none() || !create_issued) {
            return Err(Error::new(format!(
                "release journal carries {label}upload intents without its durable {label}draft capability"
            )));
        }
        Ok(())
    }

    /// The first [`STEPS`] entry not yet journaled — where `--resume` re-enters.
    /// `None` ⇒ the cut completed. Older formats walk the step list they were
    /// written against, so a completed journal stays completed across a format
    /// bump that inserted a step (`mirror`, in format 6) or removed one
    /// (`cask`, in format 7).
    pub fn first_incomplete(&self) -> Option<&'static str> {
        let steps: &[&'static str] = match self.format {
            1 => &LEGACY_STEPS,
            ..=5 => &STEPS_V5,
            6 => &STEPS_V6,
            _ => &STEPS,
        };
        steps.iter().copied().find(|step| !self.is_done(step))
    }

    /// Older formats did not record every current authority (most recently
    /// the immutable GitHub release ID). A partially completed old cut cannot
    /// safely enter current mutations and must use stopped-publisher recovery.
    pub fn ensure_resumable(&self) -> Result<()> {
        if self.format < JOURNAL_FORMAT && self.first_incomplete().is_some() {
            return Err(Error::new(format!(
                "legacy release journal format {} for v{} (build {}) is unfinished and cannot \
                 be resumed safely: it predates the current publisher/signing/release-ID \
                 capability protocol; after proving the old publisher stopped, use \
                 `cargo ship recover v{} {} --old-publisher-stopped` from a trusted machine",
                self.format, self.version, self.build_number, self.version, self.commit
            )));
        }
        Ok(())
    }

    /// Record a completed step and persist immediately — the journal is only
    /// trustworthy if it never lags the world by more than the in-flight step.
    pub fn mark(&mut self, step: &str, path: &Path) -> Result<()> {
        if !self.is_done(step) {
            self.done.push(step.to_string());
        }
        self.save(path)
    }
}

// ---------------------------------------------------------------------------
// pure publish helpers (tested in tests/resume.rs)
// ---------------------------------------------------------------------------

/// Admission decision for a non-idempotent remote POST whose response may be
/// lost. The durable intent is deliberately conservative: once persisted, an
/// absent object means "wait/discover", never "try the POST again". Visibility
/// always converges through the immutable object instead of issuing a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurablePostDecision {
    ConvergeVisible,
    PersistIntentThenPost,
    AwaitVisibility,
}

#[must_use]
pub const fn durable_post_decision(
    durable_intent_issued: bool,
    exact_object_visible: bool,
) -> DurablePostDecision {
    if exact_object_visible {
        DurablePostDecision::ConvergeVisible
    } else if durable_intent_issued {
        DurablePostDecision::AwaitVisibility
    } else {
        DurablePostDecision::PersistIntentThenPost
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbsentDraftDecision {
    AbandonProvenNoPost,
    RetainOwnerAwaitVisibility,
}

/// An absent listing is destructive-cleanup authority only when a current
/// durable journal proves no create POST was ever issued. `None` represents a
/// lost/legacy journal and is deliberately as unsafe as a known issued intent.
#[must_use]
pub const fn absent_draft_decision(durable_create_intent: Option<bool>) -> AbsentDraftDecision {
    match durable_create_intent {
        Some(false) => AbsentDraftDecision::AbandonProvenNoPost,
        Some(true) | None => AbsentDraftDecision::RetainOwnerAwaitVisibility,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftCleanupDecision {
    AbandonProvenNoPost,
    DeleteIssuedVisible,
    RetainIssuedAwaitVisibility,
    RefuseUnknownOrInconsistent,
}

#[must_use]
pub const fn draft_cleanup_decision(
    durable_create_intent: Option<bool>,
    exact_draft_visible: bool,
) -> DraftCleanupDecision {
    match (durable_create_intent, exact_draft_visible) {
        (Some(false), false) => DraftCleanupDecision::AbandonProvenNoPost,
        (Some(true), true) => DraftCleanupDecision::DeleteIssuedVisible,
        (Some(true), false) => DraftCleanupDecision::RetainIssuedAwaitVisibility,
        (Some(false), true) | (None, false) | (None, true) => {
            DraftCleanupDecision::RefuseUnknownOrInconsistent
        }
    }
}

/// Process-local, non-cloneable authority to issue exactly one remote POST.
/// It is minted only after the corresponding intent journal save returns from
/// its file + directory fsync boundary; a crash necessarily destroys it.
pub(crate) struct DurablePostPermit(());

impl Drop for DurablePostPermit {
    fn drop(&mut self) {}
}

fn issue_nonidempotent_post(_permit: DurablePostPermit, args: &[&str]) -> Result<RunOut> {
    let out = Command::new("curl")
        .args(args)
        .output()
        .map_err(|error| Error::new(format!("spawn one-shot curl POST: {error}")))?;
    Ok(RunOut {
        status: out.status.code().unwrap_or(-1),
        stdout: out.stdout,
        stderr: out.stderr,
    })
}

struct GithubAuthHeaders {
    _dir: PrivateTempDir,
    curl_header_arg: String,
}

pub const GITHUB_AUTH_HOST: &str = "github.com";
pub const GITHUB_API_ORIGIN: &str = "https://api.github.com";
pub const GITHUB_UPLOAD_ORIGIN: &str = "https://uploads.github.com";

#[must_use]
pub const fn github_auth_token_args() -> [&'static str; 4] {
    ["auth", "token", "--hostname", GITHUB_AUTH_HOST]
}

pub fn validate_one_shot_curl_help(help: &str) -> Result<()> {
    for option in [
        "--data-binary",
        "--fail-with-body",
        "--header",
        "--request",
        "--retry",
        "--show-error",
        "--silent",
        "--url",
    ] {
        if !help
            .split_whitespace()
            .any(|token| token.trim_matches(',') == option)
        {
            return Err(Error::new(format!(
                "curl transport lacks required one-shot POST option {option}"
            )));
        }
    }
    Ok(())
}

/// Prove the curl binary supports every one-shot POST option. The option set
/// cannot change within one process, so the probe runs once and every later
/// caller sees the same verdict (including the original failure, verbatim).
fn curl_transport_preflight() -> Result<()> {
    static VERDICT: std::sync::OnceLock<std::result::Result<(), String>> =
        std::sync::OnceLock::new();
    VERDICT
        .get_or_init(|| {
            let curl = Command::new("curl")
                .args(["--help", "all"])
                .output()
                .map_err(|error| format!("spawn curl transport preflight: {error}"))?;
            if !curl.status.success() {
                return Err("curl transport preflight failed before durable POST intent".into());
            }
            let curl_help = std::str::from_utf8(&curl.stdout)
                .map_err(|_| "curl transport help is not UTF-8".to_string())?;
            validate_one_shot_curl_help(curl_help).map_err(|error| error.to_string())
        })
        .clone()
        .map_err(Error::new)
}

fn prepare_github_auth_headers() -> Result<GithubAuthHeaders> {
    curl_transport_preflight()?;
    // Under a channel scope the upload targets the PUBLIC channel, which `gh auth`
    // cannot write; use the release-org token for the header file instead. Outside
    // the scope this is unchanged.
    let owned = match active_channel_token() {
        Some(token) => token,
        None => {
            let out = Command::new("gh")
                .args(github_auth_token_args())
                .output()
                .map_err(|error| Error::new(format!("spawn GitHub token preflight: {error}")))?;
            if !out.status.success() {
                return Err(Error::new(
                    "GitHub authentication token is unavailable before durable POST intent",
                ));
            }
            std::str::from_utf8(&out.stdout)
                .map_err(|_| Error::new("GitHub authentication token is not UTF-8"))?
                .trim()
                .to_string()
        }
    };
    let token = owned.as_str();
    if token.is_empty() || token.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(Error::new(
            "GitHub authentication token is empty or non-canonical",
        ));
    }
    let dir = PrivateTempDir::create(std::env::temp_dir().join(format!(
        "aterm-release-auth-{}-{}",
        std::process::id(),
        RELEASE_ASSET_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )))?;
    let header_path = dir.path().join("headers");
    fs::write(
        &header_path,
        format!(
            "Authorization: Bearer {token}\nAccept: application/vnd.github+json\nX-GitHub-Api-Version: 2022-11-28\n"
        ),
    )
    .map_err(|error| Error::new(format!("write private GitHub auth headers: {error}")))?;
    let header_path = header_path
        .to_str()
        .ok_or_else(|| Error::new("private GitHub auth-header path is not UTF-8"))?;
    Ok(GithubAuthHeaders {
        _dir: dir,
        curl_header_arg: format!("@{header_path}"),
    })
}

/// A fully prepared one-shot POST. Every fallible preflight — the private
/// payload file, the auth-header file, argv encoding — completes at
/// construction, BEFORE the caller persists its durable intent; the
/// permit-consuming [`Self::issue`] then goes straight to curl. The held temp
/// dirs keep the payload and header files alive until the POST returns.
struct OneShotPost {
    _payload_dir: Option<PrivateTempDir>,
    _auth: GithubAuthHeaders,
    args: Vec<String>,
}

impl OneShotPost {
    /// JSON-body POST (draft creates). `temp_label` distinguishes the
    /// private/mirror temp directories; `subject` names the request in errors.
    fn prepare_json(
        temp_label: &str,
        subject: &str,
        endpoint: &str,
        payload: &[u8],
    ) -> Result<Self> {
        let payload_dir = PrivateTempDir::create(std::env::temp_dir().join(format!(
            "aterm-release-{temp_label}-{}-{}",
            std::process::id(),
            RELEASE_ASSET_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        )))?;
        let payload_path = payload_dir.path().join("request.json");
        fs::write(&payload_path, payload)
            .map_err(|error| Error::new(format!("write {subject}: {error}")))?;
        let payload_arg = payload_path
            .to_str()
            .ok_or_else(|| Error::new(format!("{subject} path is not UTF-8")))?;
        let data_arg = format!("@{payload_arg}");
        let auth = prepare_github_auth_headers()?;
        Ok(Self {
            args: Self::curl_args(&auth, "Content-Type: application/json", &data_arg, endpoint),
            _payload_dir: Some(payload_dir),
            _auth: auth,
        })
    }

    /// Raw file-body POST (asset uploads). `subject` names the file in errors.
    fn prepare_binary(subject: &str, endpoint: &str, file: &Path) -> Result<Self> {
        let file_arg = file
            .to_str()
            .ok_or_else(|| Error::new(format!("{subject} path is not UTF-8")))?;
        let data_arg = format!("@{file_arg}");
        let auth = prepare_github_auth_headers()?;
        Ok(Self {
            args: Self::curl_args(
                &auth,
                "Content-Type: application/octet-stream",
                &data_arg,
                endpoint,
            ),
            _payload_dir: None,
            _auth: auth,
        })
    }

    fn curl_args(
        auth: &GithubAuthHeaders,
        content_type: &str,
        data_arg: &str,
        endpoint: &str,
    ) -> Vec<String> {
        [
            "--silent",
            "--show-error",
            "--fail-with-body",
            "--retry",
            "0",
            "--request",
            "POST",
            "--header",
            &auth.curl_header_arg,
            "--header",
            content_type,
            "--data-binary",
            data_arg,
            "--url",
            endpoint,
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    fn issue(self, permit: DurablePostPermit) -> Result<RunOut> {
        let args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        issue_nonidempotent_post(permit, &args)
    }
}

/// Canonical release-channel floor. Zero has the same semantics as absence,
/// so cuts never start emitting `min_build = 0` into a channel that previously
/// omitted the optional key.
fn canonical_min_build(floor: Option<u64>) -> Option<u64> {
    floor.filter(|floor| *floor != 0)
}

fn display_floor(floor: Option<u64>) -> String {
    canonical_min_build(floor).map_or_else(|| "absent".to_string(), |floor| floor.to_string())
}

fn validate_min_build(floor: Option<u64>, build: u64, subject: &str) -> Result<Option<u64>> {
    let floor = canonical_min_build(floor);
    if let Some(floor) = floor
        && floor > build
    {
        return Err(Error::new(format!(
            "min_build floor {floor} exceeds the {subject} {build}; refusing to publish an \
             impossible update floor"
        )));
    }
    Ok(floor)
}

/// Resolve the floor for a newly claimed build. This is the single production
/// policy used before claim (against the provisional number), after claim
/// (against the verified number), in the manifest context, and in the journal:
/// floors only rise, zero stays absent, and no floor may exceed its own build.
pub fn effective_min_build(
    operator: Option<u64>,
    newest_channel: Option<u64>,
    claimed_build: u64,
) -> Result<Option<u64>> {
    let floor = operator.unwrap_or(0).max(newest_channel.unwrap_or(0));
    validate_min_build(Some(floor), claimed_build, "newly claimed build")
}

/// Late race guard: every self-check/pre-flip/flip replay must still cover the
/// newest manifest's floor. If another cut raised it after our initial scan,
/// this cut remains invisible and must be recut rather than lowering the
/// channel ratchet.
pub fn channel_floor_covered(carried: Option<u64>, newest_channel: Option<u64>) -> Result<()> {
    let carried = canonical_min_build(carried).unwrap_or(0);
    let newest = canonical_min_build(newest_channel).unwrap_or(0);
    if newest > carried {
        return Err(Error::new(format!(
            "channel floor advanced to min_build {newest}, but this cut carries {carried}; \
             refusing to lower the ratchet — recut to inherit the current channel floor"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// single-head appcast archive migration (pure plan + injected executor)
// ---------------------------------------------------------------------------

/// One GitHub release asset relevant to appcast channel migration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AppcastAsset {
    pub id: u64,
    pub name: String,
}

/// The relevant assets on one release. Drafts remain represented so planning
/// can prove they are skipped rather than relying on the API query to hide them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AppcastRelease {
    pub release_id: u64,
    pub tag: String,
    pub draft: bool,
    pub target_commitish: String,
    pub assets: Vec<AppcastAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SignedChannelAsset {
    release_id: u64,
    tag: String,
    manifest_asset_id: u64,
    manifest_name: String,
    signature_asset_id: u64,
    signature_name: String,
}

/// Enumerate every published signature together with the manifest bytes it
/// authenticates.  Both live exact names and deterministic archived names are
/// channel history; looking only at the current exact name would let archive
/// migration silently reset the signing ratchet.
fn signed_channel_assets(releases: &[AppcastRelease]) -> Result<Vec<SignedChannelAsset>> {
    let mut signed = Vec::new();
    for release in releases.iter().filter(|release| !release.draft) {
        let archived_manifest = manifest_out::archived_manifest_asset(&release.tag);
        let archived_signature = manifest_out::archived_manifest_signature_asset(&release.tag);
        let exact_manifest = unique_asset_id(release, manifest_out::MANIFEST_ASSET)?;
        let archived_manifest_id = unique_asset_id(release, &archived_manifest)?;
        let exact_signature = unique_asset_id(release, manifest_out::MANIFEST_SIG_ASSET)?;
        let archived_signature_id = unique_asset_id(release, &archived_signature)?;
        if exact_signature.is_some() && archived_signature_id.is_some() {
            return Err(Error::new(format!(
                "published release {} has both exact and archived manifest signatures",
                release.tag
            )));
        }
        let signature_is_exact = exact_signature.is_some();
        let signature_name = if signature_is_exact {
            Some(manifest_out::MANIFEST_SIG_ASSET.to_string())
        } else if archived_signature_id.is_some() {
            Some(archived_signature)
        } else {
            None
        };
        if let Some(signature_name) = signature_name {
            // During archive convergence the manifest is renamed before its
            // signature. Prefer the same naming tier as the signature, then
            // the other tier for that one valid transitional state. If both
            // manifests exist, the archive planner separately rejects the
            // name collision before any PATCH; pairing remains deterministic.
            let (manifest_name, manifest_asset_id) =
                if signature_is_exact && let Some(id) = exact_manifest {
                    (manifest_out::MANIFEST_ASSET.to_string(), id)
                } else if !signature_is_exact && let Some(id) = archived_manifest_id {
                    (archived_manifest, id)
                } else if let Some(id) = exact_manifest {
                    (manifest_out::MANIFEST_ASSET.to_string(), id)
                } else if let Some(id) = archived_manifest_id {
                    (archived_manifest, id)
                } else {
                    return Err(Error::new(format!(
                        "published release {} has signature {signature_name} without an exact \
                         or archived paired manifest",
                        release.tag
                    )));
                };
            signed.push(SignedChannelAsset {
                release_id: release.release_id,
                tag: release.tag.clone(),
                manifest_asset_id,
                manifest_name,
                signature_asset_id: exact_signature
                    .or(archived_signature_id)
                    .expect("signature name implies asset ID"),
                signature_name,
            });
        }
    }
    Ok(signed)
}

/// The signing ratchet is retired: signing is never REQUIRED by published
/// history. Older releases may still carry `.sig` assets, but an unsigned
/// successor is always permitted (Tier REPO). This still validates that the
/// signed-asset inventory is internally consistent (duplicate/orphan pairs are
/// hard errors) so the archive planner sees coherent metadata; the verdict it
/// returns to publish/archive decisions is unconditionally "not required".
#[allow(dead_code)] // Public pure Tier-1/integration-test seam.
pub fn channel_signature_required(releases: &[AppcastRelease]) -> Result<bool> {
    // Surface any metadata inconsistency (e.g. exact + archived signature on one
    // release) as an error, but never force a signed successor.
    let _ = signed_channel_assets(releases)?;
    Ok(false)
}

/// One reversible metadata-only rename. `id` binds the operation to the same
/// stored bytes; production changes only the asset's `name` via REST PATCH.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppcastRename {
    pub release_id: u64,
    pub tag: String,
    pub target_commitish: String,
    pub id: u64,
    pub from: String,
    pub to: String,
}

/// Injected remote boundary. Production uses GitHub GET + metadata PATCH;
/// tests use an in-memory implementation that can crash between renames.
pub trait AppcastArchiveRemote {
    fn list_releases(&mut self) -> Result<Vec<AppcastRelease>>;
    fn rename_asset(&mut self, rename: &AppcastRename) -> Result<()>;
}

fn unique_asset_id(release: &AppcastRelease, name: &str) -> Result<Option<u64>> {
    let mut ids = release
        .assets
        .iter()
        .filter(|asset| asset.name == name)
        .map(|asset| asset.id);
    let first = ids.next();
    if ids.next().is_some() {
        return Err(Error::new(format!(
            "release {} has duplicate assets named {name}; refusing an ambiguous archive",
            release.tag
        )));
    }
    Ok(first)
}

/// What one published release tag is to the CURRENT version protocol. The
/// publisher's classification IS the client's (`aterm-update/src/github.rs`):
/// both compile the one grammar in [`aterm_update_core::tag`], so publisher and
/// fleet cannot disagree about which releases are even candidates.
pub use aterm_update_core::tag::TagKind;

/// Classify one release tag.
///
/// The grammar is [`aterm_update_core::tag::parse_release_tag`]; only the
/// publisher's diagnostic wording is here. Only the canonical three-component
/// `vMAJOR.MINOR.PATCH` spelling is a candidate. Exactly two components are the
/// retired scheme ([`TagKind::Legacy`]). Anything else — non-numeric, empty or
/// leading-zero components, a bare `v0`, more than three components — is a hard
/// error: garbage in the tag namespace must fail closed rather than silently
/// narrow the candidate set.
pub fn parse_release_tag(tag: &str) -> Result<TagKind> {
    aterm_update_core::tag::parse_release_tag(tag).map_err(|error| {
        Error::new(match error {
            TagError::Malformed => {
                format!("published appcast tag {tag:?} is not numeric dotted vN.N.N")
            }
            TagError::Overflow => {
                format!("published appcast tag {tag:?} has an overflowing numeric component")
            }
        })
    })
}

/// Parse the release protocol's canonical `vMAJOR.MINOR.PATCH` tag into a
/// numeric order. GitHub's list-releases endpoint documents no response
/// ordering, so channel authority must come from aterm's own version protocol
/// rather than the position of a REST row.
///
/// A retired two-component tag is NOT canonical authority — callers that must
/// tolerate the published archive classify with [`parse_release_tag`] first.
pub fn canonical_channel_tag_order(tag: &str) -> Result<(u64, u64, u64)> {
    let not_canonical = || {
        Error::new(format!(
            "published appcast tag {tag:?} is not canonical vMAJOR.MINOR.PATCH"
        ))
    };
    let TagKind::Candidate(components) = parse_release_tag(tag)? else {
        return Err(not_canonical());
    };
    // `parse_release_tag` already refused non-canonical spellings; the shared
    // pin re-derives the string, tying the spelling to this exact tag too.
    if aterm_update_core::tag::canonical_version(tag, &components).is_none() {
        return Err(not_canonical());
    }
    let [major, minor, patch] = components.as_slice() else {
        return Err(not_canonical());
    };
    Ok((*major, *minor, *patch))
}

/// The canonical version string carried by a canonical release tag:
/// `"v0.2.0"` → `"0.2.0"`.
pub fn canonical_channel_tag_version(tag: &str) -> Result<String> {
    let (major, minor, patch) = canonical_channel_tag_order(tag)?;
    Ok(format!("{major}.{minor}.{patch}"))
}

/// Establish that the caller still owns the intended channel head before any
/// historical metadata is touched. A stale v0.2.0 journal must never archive a
/// subsequently published v0.3.0 head; comparing canonical channel versions is
/// independent of GitHub's undocumented list order.
fn prove_archive_authority<'a>(
    releases: &'a [AppcastRelease],
    current_tag: &str,
    current_signature_required: bool,
) -> Result<&'a AppcastRelease> {
    let (current_major, current_minor, current_patch) = canonical_channel_tag_order(current_tag)?;
    let current_order = vec![current_major, current_minor, current_patch];
    let current: Vec<&AppcastRelease> = releases
        .iter()
        .filter(|release| !release.draft && release.tag == current_tag)
        .collect();
    if current.len() != 1 {
        return Err(Error::new(format!(
            "archive requires exactly one published current release {current_tag}; found {}",
            current.len()
        )));
    }
    let current = current[0];
    if unique_asset_id(current, manifest_out::MANIFEST_ASSET)?.is_none() {
        return Err(Error::new(format!(
            "published current release {current_tag} does not carry the exact channel head {}",
            manifest_out::MANIFEST_ASSET
        )));
    }
    if current_signature_required
        && unique_asset_id(current, manifest_out::MANIFEST_SIG_ASSET)?.is_none()
    {
        return Err(Error::new(format!(
            "signed channel head {current_tag} has no exact {}; refusing to hide every older \
             signed candidate",
            manifest_out::MANIFEST_SIG_ASSET
        )));
    }

    for release in releases.iter().filter(|release| !release.draft) {
        let carries_exact = unique_asset_id(release, manifest_out::MANIFEST_ASSET)?.is_some()
            || unique_asset_id(release, manifest_out::MANIFEST_SIG_ASSET)?.is_some();
        if carries_exact && release.tag != current_tag {
            // A retired two-component release can never be newer than a
            // current-scheme head: it is not on this version line at all. It
            // is still archived below (its exact asset leaves the client's
            // discovery surface) — it just does not contest authority.
            let TagKind::Candidate(release_order) = parse_release_tag(&release.tag)? else {
                continue;
            };
            if release_order >= current_order {
                return Err(Error::new(format!(
                    "refusing stale archive for {current_tag}: same-or-newer published channel \
                     tag {} still carries an exact appcast asset",
                    release.tag
                )));
            }
        }
    }
    Ok(current)
}

/// Build the complete migration plan BEFORE the first mutation. Existing
/// archive targets alongside exact-name sources are hard collisions; a source
/// already absent with its archive target present is a successfully completed
/// prefix from an interrupted prior run.
#[allow(dead_code)] // Public pure Tier-1/integration-test seam.
pub fn plan_appcast_archive(
    releases: &[AppcastRelease],
    current_tag: &str,
) -> Result<Vec<AppcastRename>> {
    plan_appcast_archive_with_policy(releases, current_tag, channel_signature_required(releases)?)
}

fn plan_appcast_archive_with_policy(
    releases: &[AppcastRelease],
    current_tag: &str,
    current_signature_required: bool,
) -> Result<Vec<AppcastRename>> {
    prove_archive_authority(releases, current_tag, current_signature_required)?;

    let mut plan = Vec::new();
    for release in releases {
        if release.draft || release.tag == current_tag {
            continue;
        }
        let archived_manifest = manifest_out::archived_manifest_asset(&release.tag);
        let archived_signature = manifest_out::archived_manifest_signature_asset(&release.tag);
        for (from, to) in [
            (manifest_out::MANIFEST_ASSET, archived_manifest.as_str()),
            (
                manifest_out::MANIFEST_SIG_ASSET,
                archived_signature.as_str(),
            ),
        ] {
            let source = unique_asset_id(release, from)?;
            let target = unique_asset_id(release, to)?;
            match (source, target) {
                (Some(_), Some(_)) => {
                    return Err(Error::new(format!(
                        "release {} has both {from} and archive target {to}; refusing to \
                         overwrite a name collision",
                        release.tag
                    )));
                }
                (Some(id), None) => plan.push(AppcastRename {
                    release_id: release.release_id,
                    tag: release.tag.clone(),
                    target_commitish: release.target_commitish.clone(),
                    id,
                    from: from.to_string(),
                    to: to.to_string(),
                }),
                (None, _) => {}
            }
        }
    }
    Ok(plan)
}

/// Prove the converged discovery invariant: exactly the current published tag
/// owns the exact manifest name, and no historical published release retains
/// the matching exact signature name. Draft assets are intentionally outside
/// the update channel and remain untouched.
#[allow(dead_code)] // Public pure Tier-1/integration-test seam.
pub fn prove_single_appcast_head(releases: &[AppcastRelease], current_tag: &str) -> Result<()> {
    prove_single_appcast_head_with_policy(
        releases,
        current_tag,
        channel_signature_required(releases)?,
    )
}

fn prove_single_appcast_head_with_policy(
    releases: &[AppcastRelease],
    current_tag: &str,
    current_signature_required: bool,
) -> Result<()> {
    let current = prove_archive_authority(releases, current_tag, current_signature_required)?;
    let heads: Vec<&str> = releases
        .iter()
        .filter(|release| {
            !release.draft
                && release
                    .assets
                    .iter()
                    .any(|asset| asset.name == manifest_out::MANIFEST_ASSET)
        })
        .map(|release| release.tag.as_str())
        .collect();
    if heads != [current_tag] {
        return Err(Error::new(format!(
            "single-head invariant failed: exact {} is published on {:?}, expected only \
             {current_tag}",
            manifest_out::MANIFEST_ASSET,
            heads
        )));
    }
    if current_signature_required
        && unique_asset_id(current, manifest_out::MANIFEST_SIG_ASSET)?.is_none()
    {
        return Err(Error::new(format!(
            "single-head invariant failed: signed current release {current_tag} has no {}",
            manifest_out::MANIFEST_SIG_ASSET
        )));
    }
    let stale_signatures: Vec<&str> = releases
        .iter()
        .filter(|release| {
            !release.draft
                && release.tag != current_tag
                && release
                    .assets
                    .iter()
                    .any(|asset| asset.name == manifest_out::MANIFEST_SIG_ASSET)
        })
        .map(|release| release.tag.as_str())
        .collect();
    if !stale_signatures.is_empty() {
        return Err(Error::new(format!(
            "single-head invariant failed: historical exact appcast signatures remain on \
             {stale_signatures:?}"
        )));
    }
    Ok(())
}

fn prove_renames_preserved_assets(
    plan: &[AppcastRename],
    releases: &[AppcastRelease],
) -> Result<()> {
    for rename in plan {
        let release = releases
            .iter()
            .find(|release| {
                !release.draft
                    && release.release_id == rename.release_id
                    && release.tag == rename.tag
                    && release.target_commitish == rename.target_commitish
            })
            .ok_or_else(|| {
                Error::new(format!(
                    "release {} vanished while archiving appcast asset {}",
                    rename.tag, rename.id
                ))
            })?;
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.id == rename.id)
            .ok_or_else(|| {
                Error::new(format!(
                    "appcast asset {} on {} vanished instead of being metadata-renamed",
                    rename.id, rename.tag
                ))
            })?;
        if asset.name != rename.to {
            return Err(Error::new(format!(
                "appcast asset {} on {} is named {:?} after PATCH, expected {:?}",
                rename.id, rename.tag, asset.name, rename.to
            )));
        }
    }
    Ok(())
}

/// Execute a complete preflighted plan, then re-list and prove both byte-object
/// preservation (same asset IDs under archive names) and the sole exact head.
/// A crash leaves the journal at `archive`; the next run plans only the
/// unfinished suffix because successful metadata renames are already visible.
#[allow(dead_code)] // Public pure Tier-1/integration-test seam.
pub fn converge_appcast_archive(
    remote: &mut impl AppcastArchiveRemote,
    current_tag: &str,
) -> Result<usize> {
    let before = remote.list_releases()?;
    let required = channel_signature_required(&before)?;
    converge_appcast_archive_from_listing(remote, current_tag, required, before)
}

/// Converge an already identity-validated release under its epoch policy.
/// This differs from [`converge_appcast_archive`] only for the explicitly
/// supported v0.27-v0.54 unsigned-bootstrap recovery epoch: signed v0.26
/// history must not make that unsigned *historical* head impossible to
/// archive. When signing is configured, callers verify the current
/// manifest/signature pair under the configured key before this call; an
/// unsigned channel has no such pair to check.
pub fn converge_appcast_archive_with_policy(
    remote: &mut impl AppcastArchiveRemote,
    current_tag: &str,
    current_signature_required: bool,
) -> Result<usize> {
    let before = remote.list_releases()?;
    converge_appcast_archive_from_listing(remote, current_tag, current_signature_required, before)
}

fn converge_appcast_archive_from_listing(
    remote: &mut impl AppcastArchiveRemote,
    current_tag: &str,
    current_signature_required: bool,
    before: Vec<AppcastRelease>,
) -> Result<usize> {
    let plan = plan_appcast_archive_with_policy(&before, current_tag, current_signature_required)?;
    for rename in &plan {
        remote.rename_asset(rename)?;
    }
    let after = remote.list_releases()?;
    prove_renames_preserved_assets(&plan, &after)?;
    prove_single_appcast_head_with_policy(&after, current_tag, current_signature_required)?;
    Ok(plan.len())
}

const APPCAST_ASSET_LIST_JQ: &str = r#".[] | . as $r |
    ("aterm-appcast-" + $r.tag_name + ".toml") as $archive |
    {release_id: $r.id,
     tag: $r.tag_name,
     draft: $r.draft,
     target_commitish: $r.target_commitish,
     assets: [$r.assets[]? |
       select(.name == "aterm-appcast.toml" or
              .name == "aterm-appcast.toml.sig" or
              .name == $archive or
              .name == ($archive + ".sig")) |
       {id: .id, name: .name}]}
    | @json"#;

/// Parse the bounded GitHub listing used by the production archive remote.
/// Each line represents one release even when it has no relevant assets, so
/// pagination counts releases rather than assets.
pub fn parse_appcast_asset_listing(listing: &str) -> Result<Vec<AppcastRelease>> {
    let mut releases = Vec::new();
    for (index, line) in listing.lines().enumerate() {
        let release: AppcastRelease = serde_json::from_str(line).map_err(|error| {
            Error::new(format!(
                "malformed GitHub appcast asset row {}: {error}",
                index + 1
            ))
        })?;
        if release.tag.is_empty() {
            return Err(Error::new(format!(
                "malformed GitHub appcast asset row {}: empty tag",
                index + 1
            )));
        }
        releases.push(release);
    }
    Ok(releases)
}

struct GhAppcastArchiveRemote<'a> {
    slug: &'a str,
    session: Option<ArchivePublisherSession<'a>>,
}

struct ArchivePublisherSession<'a> {
    repo: &'a Path,
    lease: &'a ReleaseLeaseGuard,
    fence: &'a PublisherFenceGuard,
}

impl<'a> GhAppcastArchiveRemote<'a> {
    fn read_only(slug: &'a str) -> Self {
        Self {
            slug,
            session: None,
        }
    }

    fn fenced(
        slug: &'a str,
        repo: &'a Path,
        lease: &'a ReleaseLeaseGuard,
        fence: &'a PublisherFenceGuard,
    ) -> Self {
        Self {
            slug,
            session: Some(ArchivePublisherSession { repo, lease, fence }),
        }
    }

    fn assert_mutation_fence(&self) -> Result<()> {
        let session = self.session.as_ref().ok_or_else(|| {
            Error::new("archive PATCH attempted without a unique publisher session")
        })?;
        assert_publisher_session(&GitCli::new(session.repo), session.lease, session.fence)
    }
}

impl AppcastArchiveRemote for GhAppcastArchiveRemote<'_> {
    fn list_releases(&mut self) -> Result<Vec<AppcastRelease>> {
        const PER_PAGE: usize = 100;
        const MAX_PAGES: u32 = 10;
        let mut releases = Vec::new();
        for page in 1..=MAX_PAGES {
            let path = format!(
                "repos/{}/releases?per_page={PER_PAGE}&page={page}",
                self.slug
            );
            let out = gh_retry(&["api", &path, "--jq", APPCAST_ASSET_LIST_JQ])?;
            let page_releases = parse_appcast_asset_listing(&out.stdout_utf8())?;
            let page_len = page_releases.len();
            releases.extend(page_releases);
            if page_len < PER_PAGE {
                break;
            }
            if page == MAX_PAGES {
                return Err(Error::new(format!(
                    "GitHub release listing reached the {MAX_PAGES}-page safety cap; cannot \
                     prove every published appcast was archived"
                )));
            }
        }
        Ok(releases)
    }

    fn rename_asset(&mut self, rename: &AppcastRename) -> Result<()> {
        let path = format!("repos/{}/releases/assets/{}", self.slug, rename.id);
        let name = format!("name={}", rename.to);
        let mut last = String::new();
        for (attempt, backoff) in [(1u32, 2u64), (2, 5), (3, 0)] {
            let release = release_object_by_id(self.slug, rename.release_id)?;
            validate_release_object_capability(
                release.as_ref(),
                rename.release_id,
                &rename.tag,
                &rename.target_commitish,
                false,
            )?;
            // The endpoint has no If-Match/source-name precondition. Re-read
            // before every retry, accept our target as timeout convergence,
            // and reject every third-party name before PATCH.
            let inventory = release_asset_inventory_for_release_id(self.slug, rename.release_id)?;
            let observed =
                release_inventory_asset_name_by_id(&inventory, rename.release_id, rename.id)?;
            if observed == rename.to {
                return Ok(());
            }
            if observed != rename.from {
                return Err(Error::new(format!(
                    "appcast asset {} on {} changed from {:?} to {observed:?} after preflight; \
                     refusing to overwrite concurrent metadata",
                    rename.id, rename.tag, rename.from
                )));
            }
            self.assert_mutation_fence()?;
            let adjacent_release = release_object_by_id(self.slug, rename.release_id)?;
            validate_release_object_capability(
                adjacent_release.as_ref(),
                rename.release_id,
                &rename.tag,
                &rename.target_commitish,
                false,
            )?;
            let adjacent_inventory =
                release_asset_inventory_for_release_id(self.slug, rename.release_id)?;
            if release_inventory_asset_name_by_id(
                &adjacent_inventory,
                rename.release_id,
                rename.id,
            )? != rename.from
            {
                return Err(Error::new(
                    "appcast source membership changed immediately before PATCH",
                ));
            }
            self.assert_mutation_fence()?;
            let out = gh_raw(&["api", "--method", "PATCH", &path, "-f", &name])?;
            if out.success() {
                return Ok(());
            }
            last = out.stderr_utf8().trim().to_string();
            if attempt < 3 {
                std::thread::sleep(std::time::Duration::from_secs(backoff));
            }
        }
        Err(Error::new(format!(
            "archive PATCH for asset {} failed after 3 fenced attempts: {last}",
            rename.id
        )))
    }
}

// ---------------------------------------------------------------------------
// cryptographic channel ratchet + exact asset reads
// ---------------------------------------------------------------------------

fn update_key_fingerprint(encoded: &str) -> Result<String> {
    let canonical = canonical_update_pubkey(encoded)?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(canonical)
        .map_err(|_| Error::new("canonical update key failed to decode for fingerprint"))?;
    Ok(sha256_bytes(&raw))
}

/// The cut's signing verdict: per-machine opt-in, unless the workspace commits
/// a channel pin ([`committed_channel_signature_policy`]). Public as the
/// integration-test seam for the pinned-channel decision table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignaturePolicy {
    pub required: bool,
    pub pubkey: Option<String>,
}

/// Fold the COMMITTED channel pin (`[workspace.metadata.aterm]
/// update_channel_pubkey`) into the per-machine opt-in signing verdict.
///
/// No pin ⇒ exactly the opt-in behavior: configured signing material signs,
/// a keyless machine cuts unsigned (Tier REPO). A pin makes signing tracked
/// channel POLICY, and both refusals fire pre-claim, before any ledger claim
/// or remote mutation: a keyless machine may not cut for a pinned channel,
/// and a configured key that is not the pinned key is refused by name.
/// v0.16.0 was published unsigned because a keyless machine treated the
/// missing per-machine opt-in as permission and nothing committed said
/// otherwise; the pin is that missing committed statement — read from the
/// manifest, never derived from published history (the retired ratchet).
pub fn committed_channel_signature_policy(
    committed_pubkey: Option<&str>,
    material_pubkey: Option<&str>,
) -> Result<SignaturePolicy> {
    let Some(committed) = committed_pubkey else {
        return Ok(match material_pubkey {
            Some(pubkey) => SignaturePolicy {
                required: true,
                pubkey: Some(canonical_update_pubkey(pubkey)?),
            },
            None => SignaturePolicy {
                required: false,
                pubkey: None,
            },
        });
    };
    let committed = canonical_update_pubkey(committed)?;
    let Some(material) = material_pubkey else {
        return Err(Error::new(format!(
            "the committed channel anchor (aterm-update-core::pins, \
             UPDATE_CHANNEL_PUBKEYS[0] = \"{committed}\") commits every cut for the \
             pinned public channel to that signature, but no signing material was \
             supplied — a keyless machine may not cut for a pinned channel; no ledger \
             claim was made. Supply the key, or unpin the channel in a tracked commit \
             (the same deliberate act as removing {} itself)",
            mirror::CHANNEL_KEY,
        )));
    };
    let material = canonical_update_pubkey(material)?;
    if material != committed {
        return Err(Error::new(format!(
            "the configured signing key's public identity {material} is not the \
             committed channel anchor {committed} (aterm-update-core::pins, \
             UPDATE_CHANNEL_PUBKEYS[0]); refusing a release the pinned channel's \
             clients would reject"
        )));
    }
    Ok(SignaturePolicy {
        required: true,
        pubkey: Some(material),
    })
}

/// Decode and re-emit the updater Ed25519 key so journal/config comparisons
/// use one canonical identity rather than textual base64 aliases.
pub fn canonical_update_pubkey(encoded: &str) -> Result<String> {
    let encoded = encoded.trim();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| Error::new("ATERM_UPDATE_PUBKEY is not valid standard base64"))?;
    if bytes.len() != 32 {
        return Err(Error::new(format!(
            "ATERM_UPDATE_PUBKEY decodes to {} bytes, not an Ed25519 32-byte public key",
            bytes.len()
        )));
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Verify raw detached Ed25519 bytes against the canonical/persisted channel
/// key.  This is the same primitive the pinned updater uses.
pub fn verify_detached_manifest_signature(
    encoded_pubkey: &str,
    manifest: &[u8],
    signature: &[u8],
) -> Result<()> {
    let canonical = canonical_update_pubkey(encoded_pubkey)?;
    let pubkey = base64::engine::general_purpose::STANDARD
        .decode(canonical)
        .map_err(|_| Error::new("canonical update public key failed to decode"))?;
    if signature.len() != 64 {
        return Err(Error::new(format!(
            "manifest signature is {} bytes, not an Ed25519 64-byte signature",
            signature.len()
        )));
    }
    UnparsedPublicKey::new(&ED25519, pubkey)
        .verify(manifest, signature)
        .map_err(|_| Error::new("manifest signature does not verify under the channel public key"))
}

/// Pure/injected verifier for the legacy updater signature ratchet. Metadata first
/// proves an exact, unique signature on the current head (never an archive-name
/// fallback); then every signed historical pair is checked under the same key.
/// The current signature may additionally be required byte-identical to the
/// local cut artifact.
#[allow(dead_code)] // negative-control seam for the optional-signing verification path
pub fn verify_channel_head_signature_with(
    releases: &[AppcastRelease],
    head_tag: &str,
    head_manifest: &[u8],
    local_head_signature: Option<&[u8]>,
    signature_pubkey: Option<&str>,
    mut fetch_asset: impl FnMut(u64, u64, &str, &str) -> Result<Vec<u8>>,
) -> Result<bool> {
    let signed = signed_channel_assets(releases)?;
    // A trusted local/compiled key activates Tier SIG even if an attacker (or
    // broken archive) removed every remote `.sig` asset. Remote absence can
    // never reset a pin that installed updaters already enforce.
    if signed.is_empty() && signature_pubkey.is_none() {
        return Ok(false);
    }
    let pubkey = signature_pubkey.ok_or_else(|| {
        Error::new(
            "published signature history activates Tier SIG, but no pinned \
             ATERM_UPDATE_PUBKEY is available; verification cannot fall back to unsigned",
        )
    })?;
    let heads: Vec<&AppcastRelease> = releases
        .iter()
        .filter(|release| !release.draft && release.tag == head_tag)
        .collect();
    if heads.len() != 1 {
        return Err(Error::new(format!(
            "signature verification requires exactly one published release {head_tag}; found {}",
            heads.len()
        )));
    }
    let head = heads[0];
    if unique_asset_id(head, manifest_out::MANIFEST_ASSET)?.is_none() {
        return Err(Error::new(format!(
            "signed channel head {head_tag} has no exact {}",
            manifest_out::MANIFEST_ASSET
        )));
    }
    if unique_asset_id(head, manifest_out::MANIFEST_SIG_ASSET)?.is_none() {
        return Err(Error::new(format!(
            "signed channel head {head_tag} has no exact {}; archive-name fallback is forbidden",
            manifest_out::MANIFEST_SIG_ASSET
        )));
    }

    let head_signature = fetch_asset(
        head.release_id,
        unique_asset_id(head, manifest_out::MANIFEST_SIG_ASSET)?
            .expect("checked exact head signature"),
        head_tag,
        manifest_out::MANIFEST_SIG_ASSET,
    )?;
    if let Some(local) = local_head_signature
        && local != head_signature
    {
        return Err(Error::new(
            "published manifest signature is not byte-identical to the local cut artifact",
        ));
    }
    verify_detached_manifest_signature(pubkey, head_manifest, &head_signature).map_err(
        |error| {
            Error::new(format!(
                "signed channel head {head_tag} is invalid under the pinned public key: {error}"
            ))
        },
    )?;

    for asset in signed {
        if asset.tag == head_tag
            && asset.manifest_name == manifest_out::MANIFEST_ASSET
            && asset.signature_name == manifest_out::MANIFEST_SIG_ASSET
        {
            continue;
        }
        let manifest = fetch_asset(
            asset.release_id,
            asset.manifest_asset_id,
            &asset.tag,
            &asset.manifest_name,
        )?;
        let signature = fetch_asset(
            asset.release_id,
            asset.signature_asset_id,
            &asset.tag,
            &asset.signature_name,
        )?;
        verify_detached_manifest_signature(pubkey, &manifest, &signature).map_err(|error| {
            Error::new(format!(
                "signed channel history {} / {} is invalid under the pinned public key: {error}",
                asset.tag, asset.signature_name
            ))
        })?;
    }
    Ok(true)
}

/// Live wrapper used by both cut-final verification and `cargo ship verify`.
///
/// Tier REPO model: with no configured/journaled update key the channel is
/// unsigned and published signature history NEVER forces a signed successor.
/// When a key IS configured, the exact live head signature is verified under
/// it (and byte-compared against the local cut artifact during a live cut).
pub fn verify_live_channel_head_signature(
    _repo: &Path,
    slug: &str,
    head_tag: &str,
    head_manifest: &[u8],
    local_head_signature: Option<&[u8]>,
    journal_pubkey: Option<&str>,
) -> Result<bool> {
    let Some(journal_pubkey) = journal_pubkey else {
        // Unsigned channel: gh auth + SHA-256 + monotonic build number are the
        // trust. No ratchet — older `.sig` assets never demand a signed head.
        return Ok(false);
    };
    let pubkey = canonical_update_pubkey(journal_pubkey)?;
    let mut remote = GhAppcastArchiveRemote::read_only(slug);
    let releases = remote.list_releases()?;
    let heads: Vec<&AppcastRelease> = releases
        .iter()
        .filter(|release| !release.draft && release.tag == head_tag)
        .collect();
    let [head] = heads.as_slice() else {
        return Err(Error::new(format!(
            "signature verification requires exactly one published release {head_tag}; found {}",
            heads.len()
        )));
    };
    if unique_asset_id(head, manifest_out::MANIFEST_ASSET)?.is_none() {
        return Err(Error::new(format!(
            "signed channel head {head_tag} has no exact {}",
            manifest_out::MANIFEST_ASSET
        )));
    }
    let signature_id = unique_asset_id(head, manifest_out::MANIFEST_SIG_ASSET)?.ok_or_else(|| {
        Error::new(format!(
            "signed channel head {head_tag} has no exact {}; archive-name fallback is forbidden",
            manifest_out::MANIFEST_SIG_ASSET
        ))
    })?;
    let head_signature = download_snapshot_appcast_asset(
        slug,
        &releases,
        head.release_id,
        signature_id,
        head_tag,
        manifest_out::MANIFEST_SIG_ASSET,
    )?;
    if let Some(local) = local_head_signature
        && local != head_signature
    {
        return Err(Error::new(
            "published manifest signature is not byte-identical to the local cut artifact",
        ));
    }
    verify_detached_manifest_signature(&pubkey, head_manifest, &head_signature).map_err(
        |error| {
            Error::new(format!(
                "signed channel head {head_tag} is invalid under the configured public key: {error}"
            ))
        },
    )?;
    Ok(true)
}

fn download_snapshot_appcast_asset(
    slug: &str,
    releases: &[AppcastRelease],
    release_id: u64,
    asset_id: u64,
    tag: &str,
    name: &str,
) -> Result<Vec<u8>> {
    let rows: Vec<&AppcastRelease> = releases
        .iter()
        .filter(|release| !release.draft && release.release_id == release_id && release.tag == tag)
        .collect();
    let [snapshot] = rows.as_slice() else {
        return Err(Error::new(format!(
            "signature snapshot has {} published rows for release ID {release_id} tag {tag}",
            rows.len()
        )));
    };
    if unique_asset_id(snapshot, name)? != Some(asset_id) {
        return Err(Error::new(format!(
            "signature snapshot asset {name} does not bind immutable asset ID {asset_id}"
        )));
    }
    let before = release_object_by_id(slug, release_id)?;
    validate_release_object_capability(
        before.as_ref(),
        release_id,
        tag,
        &snapshot.target_commitish,
        false,
    )?;
    if release_asset_identity_for_release_id(slug, release_id, name)?.0 != asset_id {
        return Err(Error::new(
            "signature asset immutable identity changed after metadata snapshot",
        ));
    }
    let bytes = download_release_asset_for_release_id(slug, release_id, name)?;
    let after = release_object_by_id(slug, release_id)?;
    if after != before {
        return Err(Error::new(
            "signature release tag/target/state changed during exact-ID download",
        ));
    }
    Ok(bytes)
}

fn signer_tool(repo: &Path) -> Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join("atpkg-keys")));
    }
    candidates.push(repo.join("target/release/atpkg-keys"));
    if let Some(tool) = candidates.into_iter().find(|path| path.is_file()) {
        return Ok(tool);
    }
    Err(Error::new(
        "update signing is configured (~/.aterm/release.conf names a signing key), but \
         atpkg-keys is unavailable to sign the manifest. Build the owner tool with \
         `cargo build --release -p atpkg-keys`, or remove the signing configuration to cut \
         an unsigned release (signing is optional)",
    ))
}

struct SigningMaterial {
    tool: PathBuf,
    key_path: String,
    pubkey: String,
}

fn load_signing_material(repo: &Path) -> Result<Option<SigningMaterial>> {
    let conf = sign::load_default()?;
    let configured_pubkey = conf
        .as_ref()
        .and_then(|conf| conf.get("ATERM_UPDATE_PUBKEY"))
        .filter(|value| !value.is_empty());
    let key_path = conf
        .as_ref()
        .and_then(|conf| conf.get("ATERM_UPDATE_SIGN_KEY"))
        .filter(|value| !value.is_empty());
    if configured_pubkey.is_none() && key_path.is_none() {
        return Ok(None);
    }
    let (Some(configured_pubkey), Some(key_path)) = (configured_pubkey, key_path) else {
        return Err(Error::new(
            "Tier-SIG configuration is incomplete: both ATERM_UPDATE_PUBKEY and \
             ATERM_UPDATE_SIGN_KEY are required; no ledger claim was made",
        ));
    };
    let configured_pubkey = canonical_update_pubkey(configured_pubkey)?;
    let tool = signer_tool(repo)?;
    let derived = Command::new(&tool)
        .arg("pubkey")
        .arg(key_path)
        .output()
        .map_err(|error| Error::new(format!("spawn {} pubkey: {error}", tool.display())))?;
    if !derived.status.success() {
        return Err(Error::new(
            "atpkg-keys could not derive the configured signing key's public identity \
             (private key path and tool stderr suppressed); recover the offline key before cutting",
        ));
    }
    let actual_pubkey = canonical_update_pubkey(
        std::str::from_utf8(&derived.stdout)
            .map_err(|_| Error::new("atpkg-keys pubkey returned non-UTF-8 output"))?
            .trim(),
    )?;
    if actual_pubkey != configured_pubkey {
        return Err(Error::new(
            "ATERM_UPDATE_PUBKEY does not match the actual configured signing key; refusing \
             key substitution or an unverifiable release (key values suppressed)",
        ));
    }
    Ok(Some(SigningMaterial {
        tool,
        key_path: key_path.to_string(),
        pubkey: actual_pubkey,
    }))
}

const MAX_SMALL_RELEASE_ASSET_BYTES: u64 = 256 * 1024;

/// Immutable GitHub release capability. Tag names are mutable and draft tags
/// are not unique; every mutating path must carry this numeric object ID and
/// revalidate the object's tag/state/target immediately before mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseObjectIdentity {
    pub id: u64,
    pub tag: String,
    pub draft: bool,
    pub target_commitish: String,
}

pub fn parse_release_object_response(bytes: &[u8]) -> Result<ReleaseObjectIdentity> {
    #[derive(Deserialize)]
    struct Response {
        id: u64,
        tag_name: String,
        draft: bool,
        target_commitish: String,
    }
    let response: Response = serde_json::from_slice(bytes)
        .map_err(|error| Error::new(format!("parse GitHub release POST response: {error}")))?;
    if response.id == 0 || response.tag_name.is_empty() || response.target_commitish.is_empty() {
        return Err(Error::new(
            "GitHub release POST response has an empty/zero capability field",
        ));
    }
    Ok(ReleaseObjectIdentity {
        id: response.id,
        tag: response.tag_name,
        draft: response.draft,
        target_commitish: response.target_commitish,
    })
}

pub fn parse_release_object_identity_rows(rows: &str) -> Result<Vec<ReleaseObjectIdentity>> {
    rows.lines()
        .enumerate()
        .map(|(index, line)| {
            let mut fields = line.split('\t');
            let (Some(id), Some(tag), Some(draft), Some(target), None) = (
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            ) else {
                return Err(Error::new(format!(
                    "malformed GitHub release identity row {}",
                    index + 1
                )));
            };
            let id = id.parse::<u64>().map_err(|_| {
                Error::new(format!(
                    "GitHub release identity row {} has non-numeric ID",
                    index + 1
                ))
            })?;
            if id == 0 || tag.is_empty() || target.is_empty() {
                return Err(Error::new(format!(
                    "GitHub release identity row {} has an empty/zero identity field",
                    index + 1
                )));
            }
            let draft = match draft {
                "true" => true,
                "false" => false,
                _ => {
                    return Err(Error::new(format!(
                        "GitHub release identity row {} has invalid draft flag {draft:?}",
                        index + 1
                    )));
                }
            };
            Ok(ReleaseObjectIdentity {
                id,
                tag: tag.to_string(),
                draft,
                target_commitish: target.to_string(),
            })
        })
        .collect()
}

const RELEASE_IDENTITY_OBJECT_JQ: &str =
    r#"[.id, .tag_name, (.draft | tostring), .target_commitish] | @tsv"#;
const RELEASE_IDENTITY_LIST_JQ: &str =
    r#".[] | [.id, .tag_name, (.draft | tostring), .target_commitish] | @tsv"#;

/// Pin the GitHub JSON shape at each endpoint. Collection endpoints return an
/// array and must enumerate it; exact-ID endpoints return one object. One jq
/// program cannot serve both: sharing it makes the real-cut duplicate-draft
/// preflight reject every non-empty release list.
pub(crate) const fn release_identity_jq(listing: bool) -> &'static str {
    if listing {
        RELEASE_IDENTITY_LIST_JQ
    } else {
        RELEASE_IDENTITY_OBJECT_JQ
    }
}

/// Exhaustively resolve a tag to release objects. Unlike
/// `GET /releases/tags/{tag}`, this sees duplicate drafts instead of letting
/// REST order silently choose one.
pub fn release_objects_by_tag(slug: &str, tag: &str) -> Result<Vec<ReleaseObjectIdentity>> {
    const PER_PAGE: usize = 100;
    const MAX_PAGES: u32 = 10;
    let mut matches = Vec::new();
    for page in 1..=MAX_PAGES {
        let endpoint = format!("repos/{slug}/releases?per_page={PER_PAGE}&page={page}");
        let out = gh_retry(&["api", &endpoint, "--jq", release_identity_jq(true)])?;
        let rows = parse_release_object_identity_rows(&out.stdout_utf8())?;
        let page_len = rows.len();
        matches.extend(rows.into_iter().filter(|release| release.tag == tag));
        if page_len < PER_PAGE {
            break;
        }
        if page == MAX_PAGES {
            return Err(Error::new(format!(
                "release identity listing reached the {MAX_PAGES}-page safety cap before exhaustion"
            )));
        }
    }
    Ok(matches)
}

pub fn unique_release_object_by_tag(
    slug: &str,
    tag: &str,
) -> Result<Option<ReleaseObjectIdentity>> {
    let matches = release_objects_by_tag(slug, tag)?;
    match matches.as_slice() {
        [] => Ok(None),
        [release] => Ok(Some(release.clone())),
        _ => Err(Error::new(format!(
            "release tag {tag} resolves to {} GitHub release objects; refusing ambiguous draft authority",
            matches.len()
        ))),
    }
}

pub fn release_object_by_id(slug: &str, id: u64) -> Result<Option<ReleaseObjectIdentity>> {
    let endpoint = format!("repos/{slug}/releases/{id}");
    let out = gh_raw(&["api", &endpoint, "--jq", release_identity_jq(false)])?;
    if !out.success() {
        let stderr = out.stderr_utf8();
        if stderr.contains("HTTP 404") || stderr.contains("Not Found") {
            return Ok(None);
        }
        return Err(Error::new(format!(
            "read exact GitHub release ID {id} failed: {}",
            stderr.trim()
        )));
    }
    let rows = parse_release_object_identity_rows(&out.stdout_utf8())?;
    let [identity] = rows.as_slice() else {
        return Err(Error::new(format!(
            "exact GitHub release ID {id} returned {} identity rows",
            rows.len()
        )));
    };
    if identity.id != id {
        return Err(Error::new(format!(
            "exact GitHub release endpoint {id} returned foreign ID {}",
            identity.id
        )));
    }
    Ok(Some(identity.clone()))
}

pub fn validate_release_object_capability(
    observed: Option<&ReleaseObjectIdentity>,
    expected_id: u64,
    expected_tag: &str,
    expected_commit: &str,
    expected_draft: bool,
) -> Result<()> {
    let observed = observed.ok_or_else(|| {
        Error::new(format!(
            "exact GitHub release ID {expected_id} vanished before mutation"
        ))
    })?;
    if observed.id != expected_id
        || observed.tag != expected_tag
        || !release_target_matches(&observed.target_commitish, expected_commit)
        || observed.draft != expected_draft
    {
        return Err(Error::new(format!(
            "exact GitHub release ID {expected_id} changed tag/target/state; refusing mutation"
        )));
    }
    Ok(())
}

fn release_target_matches(observed: &str, expected: &str) -> bool {
    let is_oid = |value: &str| {
        matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    };
    if is_oid(observed) && is_oid(expected) {
        observed.eq_ignore_ascii_case(expected)
    } else {
        // Git ref and branch names are case-sensitive. Never normalize a
        // symbolic release target into a different mutation capability.
        observed == expected
    }
}

/// Revalidate the complete release-object snapshot captured around an exact-ID
/// asset download. Unlike claim capability checks, this intentionally accepts
/// historical symbolic targets, but only byte-for-byte as originally seen.
pub fn validate_release_object_snapshot(
    observed: Option<&ReleaseObjectIdentity>,
    expected: &ReleaseObjectIdentity,
) -> Result<()> {
    let observed = observed.ok_or_else(|| {
        Error::new(format!(
            "exact GitHub release ID {} vanished before snapshot revalidation",
            expected.id
        ))
    })?;
    if observed != expected {
        return Err(Error::new(format!(
            "exact GitHub release ID {} changed its captured identity; refusing mutation",
            expected.id
        )));
    }
    Ok(())
}

pub fn validate_release_object_tag_state(
    observed: Option<&ReleaseObjectIdentity>,
    expected_id: u64,
    expected_tag: &str,
    expected_draft: bool,
) -> Result<()> {
    let observed = observed.ok_or_else(|| {
        Error::new(format!(
            "exact GitHub release ID {expected_id} vanished while proving tag/state"
        ))
    })?;
    if observed.id != expected_id
        || observed.tag != expected_tag
        || observed.draft != expected_draft
    {
        return Err(Error::new(format!(
            "exact GitHub release ID {expected_id} changed tag/state"
        )));
    }
    Ok(())
}

/// Bound every asset ever captured in memory. Signatures have an exact wire
/// size; manifests and provenance are deliberately tiny metadata. DMGs must
/// use the separate streamed verifier and cannot accidentally reach this path.
pub fn validate_small_release_asset_size(name: &str, size: u64) -> Result<usize> {
    let limit = if name.ends_with(".sig") {
        if size != 64 {
            return Err(Error::new(format!(
                "signature asset {name} is {size} bytes, not exactly 64"
            )));
        }
        64
    } else if name.ends_with(".toml") || name.ends_with(".txt") {
        if size == 0 || size > MAX_SMALL_RELEASE_ASSET_BYTES {
            return Err(Error::new(format!(
                "metadata asset {name} size {size} is outside 1..={MAX_SMALL_RELEASE_ASSET_BYTES}"
            )));
        }
        MAX_SMALL_RELEASE_ASSET_BYTES
    } else {
        return Err(Error::new(format!(
            "asset {name} is not bounded release metadata; use the streamed asset verifier"
        )));
    };
    usize::try_from(limit).map_err(|_| Error::new("small release-asset limit does not fit usize"))
}

/// Read at most `limit + 1` bytes so a metadata/download replacement race is
/// still memory-bounded. The extra byte distinguishes exact-bound success from
/// truncation without trusting EOF or a preflight size.
pub fn read_bounded_release_asset(mut reader: impl std::io::Read, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let take = u64::try_from(limit)
        .map_err(|_| Error::new("small release-asset limit does not fit u64"))?
        .saturating_add(1);
    reader
        .by_ref()
        .take(take)
        .read_to_end(&mut bytes)
        .map_err(|error| Error::new(format!("read bounded release asset: {error}")))?;
    if bytes.len() > limit {
        return Err(Error::new(format!(
            "release asset exceeded its {limit}-byte in-memory bound while downloading"
        )));
    }
    Ok(bytes)
}

/// Concurrently drain a child's diagnostic stream to EOF while retaining only
/// a bounded prefix. Continuing to drain after the cap prevents a noisy child
/// from blocking forever on a full stderr pipe.
pub fn drain_bounded_diagnostic(
    mut reader: impl std::io::Read,
    limit: usize,
) -> Result<(Vec<u8>, bool)> {
    let mut retained = Vec::with_capacity(limit.min(8 * 1024));
    let mut truncated = false;
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|error| Error::new(format!("read child diagnostic stream: {error}")))?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(retained.len());
        let keep = remaining.min(read);
        retained.extend_from_slice(&chunk[..keep]);
        truncated |= keep < read;
    }
    Ok((retained, truncated))
}

fn exact_release_asset_download(slug: &str, id: u64) -> Result<std::process::Child> {
    let endpoint = format!("repos/{slug}/releases/assets/{id}");
    let mut command = Command::new("gh");
    command
        .args([
            "api",
            "--method",
            "GET",
            "--header",
            "Accept: application/octet-stream",
            &endpoint,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // This is a STREAMING download, so it spawns its own child instead of going
    // through `gh_raw` — the channel credential must therefore be threaded in here
    // explicitly. Without it a read inside a `ChannelCred` scope falls back to the
    // dev account and 404s on the public channel's own assets.
    if let Some(token) = active_channel_token() {
        command.env("GH_TOKEN", token);
    }
    command
        .spawn()
        .map_err(|error| Error::new(format!("spawn exact GitHub asset-ID download: {error}")))
}

pub fn download_release_asset_for_release_id(
    slug: &str,
    release_id: u64,
    name: &str,
) -> Result<Vec<u8>> {
    let before = release_asset_identity_for_release_id(slug, release_id, name)?;
    download_release_asset_with_identity_and_recheck(slug, name, before, || {
        release_asset_identity_for_release_id(slug, release_id, name)
    })
}

fn download_release_asset_with_identity_and_recheck(
    slug: &str,
    name: &str,
    before: (u64, u64),
    mut recheck: impl FnMut() -> Result<(u64, u64)>,
) -> Result<Vec<u8>> {
    let limit = validate_small_release_asset_size(name, before.1)?;
    // Pin the transfer to the immutable asset ID observed above. A name-based
    // `gh release download` can race a delete/re-upload and return bytes from a
    // different object even when the name is unchanged.
    let mut child = exact_release_asset_download(slug, before.0)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::new("exact GitHub asset-ID download has no stdout pipe"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::new("exact GitHub asset-ID download has no stderr pipe"))?;
    let stderr_reader = std::thread::spawn(move || drain_bounded_diagnostic(stderr, 64 * 1024));
    let bytes = match read_bounded_release_asset(stdout, limit) {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stderr_reader.join();
            return Err(Error::new(format!("release asset {name}: {error}")));
        }
    };
    let status = child
        .wait()
        .map_err(|error| Error::new(format!("wait for exact GitHub asset-ID download: {error}")))?;
    let (stderr, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| Error::new("exact GitHub asset-ID stderr reader panicked"))??;
    if !status.success() {
        return Err(Error::new(format!(
            "download exact release asset {name} from {slug} failed: {}{}",
            String::from_utf8_lossy(&stderr).trim(),
            if stderr_truncated {
                " [diagnostic truncated at 65536 bytes]"
            } else {
                ""
            }
        )));
    }
    let downloaded_size = u64::try_from(bytes.len())
        .map_err(|_| Error::new("downloaded release-asset length does not fit u64"))?;
    if downloaded_size != before.1 {
        return Err(Error::new(format!(
            "release asset {name} API size {} differs from bounded download size {downloaded_size}",
            before.1
        )));
    }
    let after = recheck()?;
    if after != before {
        return Err(Error::new(format!(
            "release asset {name} identity changed during bounded download"
        )));
    }
    Ok(bytes)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedReleaseAsset {
    pub id: u64,
    pub size: u64,
    pub sha256: String,
}

// The shared bound in aterm-update-core is what the CLIENT actually enforces;
// publishing against a private copy is how the two drifted (2026-08-02 raised
// this side to 2 GiB, the client's container site kept 512 MiB, and 0.15.0
// installs could accept a manifest whose payload they could never download).
const UPDATER_MAX_DMG_BYTES: u64 = aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND;
static RELEASE_ASSET_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn validate_release_asset_download_size(size: u64) -> Result<()> {
    if size == 0 || size > UPDATER_MAX_DMG_BYTES {
        return Err(Error::new(format!(
            "release asset size {size} is outside the updater's 1..={UPDATER_MAX_DMG_BYTES}-byte download bound"
        )));
    }
    Ok(())
}

/// Copy and hash an asset without ever writing more than `limit` bytes. The
/// reader is probed for one byte beyond the bound, but that byte is rejected
/// before it reaches disk. This makes the transfer bound independent of stale
/// preflight metadata or a hostile/changing HTTP response.
pub fn copy_bounded_release_asset(
    mut reader: impl std::io::Read,
    mut writer: impl std::io::Write,
    limit: u64,
) -> Result<(u64, String)> {
    let mut total = 0_u64;
    let mut digest = Sha256::new();
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        let remaining = limit.saturating_sub(total);
        let wanted = remaining.saturating_add(1).min(chunk.len() as u64) as usize;
        let read = reader
            .read(&mut chunk[..wanted])
            .map_err(|error| Error::new(format!("read streamed release asset: {error}")))?;
        if read == 0 {
            break;
        }
        let read_u64 = u64::try_from(read)
            .map_err(|_| Error::new("release-asset read length does not fit u64"))?;
        if read_u64 > remaining {
            return Err(Error::new(format!(
                "release asset exceeded its {limit}-byte transfer bound before writing excess bytes"
            )));
        }
        writer
            .write_all(&chunk[..read])
            .map_err(|error| Error::new(format!("write streamed release asset: {error}")))?;
        digest.update(&chunk[..read]);
        total += read_u64;
    }
    writer
        .flush()
        .map_err(|error| Error::new(format!("flush streamed release asset: {error}")))?;
    let sha256 = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok((total, sha256))
}

struct PrivateTempDir {
    path: Option<PathBuf>,
}

impl PrivateTempDir {
    fn create(path: PathBuf) -> Result<Self> {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder.create(&path).map_err(|error| {
            Error::new(format!(
                "create private release-asset temp directory {}: {error}",
                path.display()
            ))
        })?;
        Ok(Self { path: Some(path) })
    }

    fn path(&self) -> &Path {
        self.path.as_deref().expect("live private temp directory")
    }

    fn cleanup(mut self) -> Result<()> {
        let path = self.path.take().expect("live private temp directory");
        fs::remove_dir_all(&path).map_err(|error| {
            Error::new(format!(
                "remove release-asset temp directory {}: {error}",
                path.display()
            ))
        })
    }
}

impl Drop for PrivateTempDir {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_dir_all(path);
        }
    }
}

pub fn parse_release_asset_identity_rows(
    rows: &str,
    tag: &str,
    name: &str,
) -> Result<Option<(u64, u64)>> {
    let matches: Vec<(u64, u64)> = rows
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let (Some(observed_name), Some(id), Some(size), None) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                return Some(Err(Error::new(
                    "malformed GitHub release-asset identity row",
                )));
            };
            if observed_name != name {
                return None;
            }
            Some(
                id.parse::<u64>()
                    .and_then(|id| size.parse::<u64>().map(|size| (id, size)))
                    .map_err(|_| Error::new("GitHub release asset has non-numeric id/size")),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let [(id, size)] = matches.as_slice() else {
        if matches.is_empty() {
            return Ok(None);
        }
        return Err(Error::new(format!(
            "release {tag} has {} assets named {name:?}; expected exactly one",
            matches.len()
        )));
    };
    if *size == 0 {
        return Err(Error::new(format!("release {tag} asset {name:?} is empty")));
    }
    Ok(Some((*id, *size)))
}

pub fn release_asset_identity_for_release_id_optional(
    slug: &str,
    release_id: u64,
    name: &str,
) -> Result<Option<(u64, u64)>> {
    let endpoint = format!("repos/{slug}/releases/{release_id}");
    let out = gh_retry(&[
        "api",
        &endpoint,
        "--jq",
        r#".assets[] | [.name, (.id | tostring), (.size | tostring)] | @tsv"#,
    ])?;
    parse_release_asset_identity_rows(
        &out.stdout_utf8(),
        &format!("release-ID:{release_id}"),
        name,
    )
}

pub fn release_asset_identity_for_release_id(
    slug: &str,
    release_id: u64,
    name: &str,
) -> Result<(u64, u64)> {
    release_asset_identity_for_release_id_optional(slug, release_id, name)?.ok_or_else(|| {
        Error::new(format!(
            "release ID {release_id} has 0 assets named {name:?}; expected exactly one"
        ))
    })
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReleaseAssetInventoryEntry {
    pub name: String,
    pub id: u64,
    pub size: u64,
}

pub fn release_asset_inventory_for_release_id(
    slug: &str,
    release_id: u64,
) -> Result<Vec<ReleaseAssetInventoryEntry>> {
    let endpoint = format!("repos/{slug}/releases/{release_id}");
    let out = gh_retry(&[
        "api",
        &endpoint,
        "--jq",
        r#".assets[] | [.name, (.id | tostring), (.size | tostring)] | @tsv"#,
    ])?;
    let mut inventory = Vec::new();
    for (index, line) in out.stdout_utf8().lines().enumerate() {
        let mut fields = line.split('\t');
        let (Some(name), Some(id), Some(size), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(Error::new(format!(
                "malformed release inventory row {}",
                index + 1
            )));
        };
        inventory.push(ReleaseAssetInventoryEntry {
            name: name.to_string(),
            id: id
                .parse()
                .map_err(|_| Error::new("release inventory asset ID is non-numeric"))?,
            size: size
                .parse()
                .map_err(|_| Error::new("release inventory asset size is non-numeric"))?,
        });
    }
    inventory.sort();
    if inventory
        .windows(2)
        .any(|pair| pair[0].name == pair[1].name)
    {
        return Err(Error::new(
            "release inventory contains duplicate exact asset names",
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    if inventory.iter().any(|asset| !ids.insert(asset.id)) {
        return Err(Error::new(
            "release inventory contains a duplicate immutable asset ID",
        ));
    }
    Ok(inventory)
}

pub fn release_inventory_asset_name_by_id(
    inventory: &[ReleaseAssetInventoryEntry],
    release_id: u64,
    asset_id: u64,
) -> Result<String> {
    let matches: Vec<&ReleaseAssetInventoryEntry> = inventory
        .iter()
        .filter(|asset| asset.id == asset_id)
        .collect();
    let [asset] = matches.as_slice() else {
        return Err(Error::new(format!(
            "release ID {release_id} contains {} assets with immutable ID {asset_id}; expected exactly one",
            matches.len()
        )));
    };
    Ok(asset.name.clone())
}

pub fn validate_draft_asset_set(
    names: &[String],
    manifest: &Manifest,
    signature_required: bool,
    provenance_name: &str,
    dsym_name: Option<&str>,
) -> Result<()> {
    let count = |name: &str| {
        names
            .iter()
            .filter(|observed| observed.as_str() == name)
            .count()
    };
    // The manifest is the authority for the zip exactly as it is for the DMG: a
    // manifest that names a container the release does not carry would publish a
    // head every client resolves and then fails to download.
    let mut exact_counts = vec![
        (manifest_out::MANIFEST_ASSET, 1usize),
        (
            manifest_out::MANIFEST_SIG_ASSET,
            usize::from(signature_required),
        ),
        (manifest.dmg.as_str(), 1usize),
        (provenance_name, 1usize),
    ];
    if let Some(zip) = manifest.zip.as_deref() {
        exact_counts.push((zip, 1usize));
    }
    for (name, expected) in exact_counts {
        let observed = count(name);
        if observed != expected {
            return Err(Error::new(format!(
                "draft artifact set carries {observed} assets named {name:?}; expected {expected}"
            )));
        }
    }
    let dmgs: Vec<&str> = names
        .iter()
        .filter(|name| name.ends_with(".dmg"))
        .map(String::as_str)
        .collect();
    if dmgs != [manifest.dmg.as_str()] {
        return Err(Error::new(format!(
            "draft artifact set has non-canonical DMG names {dmgs:?}; expected exactly {:?}",
            manifest.dmg
        )));
    }
    let mut allowed = vec![
        manifest_out::MANIFEST_ASSET,
        manifest.dmg.as_str(),
        provenance_name,
    ];
    if let Some(zip) = manifest.zip.as_deref() {
        allowed.push(zip);
    }
    if signature_required {
        allowed.push(manifest_out::MANIFEST_SIG_ASSET);
    }
    if let Some(dsym) = dsym_name {
        allowed.push(dsym);
    }
    for observed in names {
        if !allowed.contains(&observed.as_str()) {
            return Err(Error::new(format!(
                "draft artifact set carries unexpected asset {observed:?}; stale build/debug assets cannot become visible"
            )));
        }
    }
    if names.len() != allowed.len() {
        return Err(Error::new(format!(
            "draft artifact set has {} objects, expected exact allowed set of {}",
            names.len(),
            allowed.len()
        )));
    }
    Ok(())
}

fn verify_release_asset_id_matches_local(
    slug: &str,
    release_id: u64,
    name: &str,
    local: &Path,
) -> Result<VerifiedReleaseAsset> {
    let before = release_asset_identity_for_release_id(slug, release_id, name)?;
    validate_release_asset_download_size(before.1)?;
    let local_size = fs::metadata(local)
        .map_err(|error| {
            Error::new(format!(
                "stat local release asset {}: {error}",
                local.display()
            ))
        })?
        .len();
    if local_size != before.1 {
        return Err(Error::new(format!(
            "release ID {release_id} asset {name} size {} differs from local size {local_size}",
            before.1
        )));
    }
    let mut child = exact_release_asset_download(slug, before.0)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::new("exact GitHub asset-ID download has no stdout pipe"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::new("exact GitHub asset-ID download has no stderr pipe"))?;
    let stderr_reader = std::thread::spawn(move || drain_bounded_diagnostic(stderr, 64 * 1024));
    let (downloaded_size, sha256) =
        match copy_bounded_release_asset(stdout, std::io::sink(), before.1) {
            Ok(value) => value,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stderr_reader.join();
                return Err(error);
            }
        };
    let status = child
        .wait()
        .map_err(|error| Error::new(format!("wait for exact GitHub asset-ID download: {error}")))?;
    let (stderr, truncated) = stderr_reader
        .join()
        .map_err(|_| Error::new("exact GitHub asset-ID stderr reader panicked"))??;
    if !status.success() {
        return Err(Error::new(format!(
            "download exact release asset ID {} failed: {}{}",
            before.0,
            String::from_utf8_lossy(&stderr).trim(),
            if truncated {
                " [diagnostic truncated]"
            } else {
                ""
            }
        )));
    }
    if downloaded_size != before.1 || dmg::sha256_file(local)? != sha256 {
        return Err(Error::new(format!(
            "release ID {release_id} asset {name} bytes differ from the local self-checked artifact"
        )));
    }
    if release_asset_identity_for_release_id(slug, release_id, name)? != before {
        return Err(Error::new(format!(
            "release ID {release_id} asset {name} identity changed during exact-ID verification"
        )));
    }
    Ok(VerifiedReleaseAsset {
        id: before.0,
        size: before.1,
        sha256,
    })
}

pub fn verify_release_asset_digest_for_release_id_to(
    slug: &str,
    release_id: u64,
    tag: &str,
    name: &str,
    expected_sha256: &str,
    destination: &Path,
) -> Result<VerifiedReleaseAsset> {
    let parent = destination
        .parent()
        .ok_or_else(|| Error::new("verified release-ID destination has no parent"))?;
    verify_release_asset_digest_inner(
        slug,
        tag,
        release_id,
        name,
        expected_sha256,
        parent,
        Some(destination),
    )
}

pub fn verify_release_asset_digest_for_release_id(
    slug: &str,
    release_id: u64,
    tag: &str,
    name: &str,
    expected_sha256: &str,
) -> Result<VerifiedReleaseAsset> {
    verify_release_asset_digest_inner(
        slug,
        tag,
        release_id,
        name,
        expected_sha256,
        &std::env::temp_dir(),
        None,
    )
}

fn verify_release_asset_digest_inner(
    slug: &str,
    tag: &str,
    release_id: u64,
    name: &str,
    expected_sha256: &str,
    temp_parent: &Path,
    retain_at: Option<&Path>,
) -> Result<VerifiedReleaseAsset> {
    let (id, size) = release_asset_identity_for_release_id(slug, release_id, name)?;
    validate_release_asset_download_size(size).map_err(|error| {
        Error::new(format!(
            "release asset {name} is not updater-downloadable: {error}"
        ))
    })?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = RELEASE_ASSET_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp_dir = temp_parent.join(format!(
        "aterm-release-asset-{}-{nonce}-{sequence}",
        std::process::id()
    ));
    let temp_dir = PrivateTempDir::create(temp_dir)?;
    let temp_asset = temp_dir.path().join("asset");
    let result = (|| -> Result<VerifiedReleaseAsset> {
        // Open before spawning so a local filesystem refusal cannot orphan a
        // downloader or its diagnostic-drain thread.
        let file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_asset)
            .map_err(|error| Error::new(format!("create streamed release asset: {error}")))?;
        let mut child = exact_release_asset_download(slug, id)?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::new("exact GitHub asset-ID download has no stdout pipe"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::new("exact GitHub asset-ID download has no stderr pipe"))?;
        let stderr_reader = std::thread::spawn(move || drain_bounded_diagnostic(stderr, 64 * 1024));
        let (downloaded_size, digest) = match copy_bounded_release_asset(stdout, file, size) {
            Ok(streamed) => streamed,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stderr_reader.join();
                return Err(Error::new(format!("release asset {name}: {error}")));
            }
        };
        let status = child.wait().map_err(|error| {
            Error::new(format!("wait for exact GitHub asset-ID download: {error}"))
        })?;
        let (stderr, stderr_truncated) = stderr_reader
            .join()
            .map_err(|_| Error::new("exact GitHub asset-ID stderr reader panicked"))??;
        if !status.success() {
            return Err(Error::new(format!(
                "download exact release asset ID {id} ({name}) from {slug}/{tag} failed: {}{}",
                String::from_utf8_lossy(&stderr).trim(),
                if stderr_truncated {
                    " [diagnostic truncated at 65536 bytes]"
                } else {
                    ""
                }
            )));
        }
        if downloaded_size != size {
            return Err(Error::new(format!(
                "release asset {name} API size {size} differs from downloaded size {downloaded_size}"
            )));
        }
        if !digest.eq_ignore_ascii_case(expected_sha256) {
            return Err(Error::new(format!(
                "release {tag} asset {name} digest {digest} does not match manifest \
                 {expected_sha256}"
            )));
        }
        // The digest covers the exact ID transfer. Re-read the name→ID/size
        // binding after hashing so a concurrent delete/re-upload cannot turn
        // verified orphan bytes into authority for a replacement object.
        let after = release_asset_identity_for_release_id(slug, release_id, name)?;
        if after != (id, size) {
            return Err(Error::new(format!(
                "release asset {name} identity changed after exact-ID download and digest"
            )));
        }
        if let Some(destination) = retain_at {
            // Recovery intentionally replaces a stale dist artifact atomically
            // with the exact-ID bytes just verified above. Subsequent archive,
            // self-check, and post-publish verification re-read this path.
            fs::rename(&temp_asset, destination).map_err(|error| {
                Error::new(format!(
                    "retain verified release asset at {}: {error}",
                    destination.display()
                ))
            })?;
        }
        Ok(VerifiedReleaseAsset {
            id,
            size,
            sha256: digest,
        })
    })();
    let cleanup = temp_dir.cleanup();
    match (result, cleanup) {
        (Ok(asset), Ok(())) => Ok(asset),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(error), Err(cleanup)) => {
            Err(Error::new(format!("{error}; cleanup failed: {cleanup}")))
        }
    }
}

fn preflight_signature_policy(repo: &Path) -> Result<SignaturePolicy> {
    // Signing is opt-in UNLESS the workspace commits a channel pin. Without
    // `[workspace.metadata.aterm] update_channel_pubkey` the channel is Tier
    // REPO (SHA-256 + monotonic build number); no signing key is required to
    // cut, a complete ~/.aterm/release.conf signs under its own key, and
    // nothing in published history can force a machine without a key to sign
    // (the ratchet is retired). WITH the pin, signing is committed channel
    // policy: a keyless machine refuses pre-claim, and a configured key that
    // is not the pinned key refuses by name. Recovery and the yank successor
    // cut route through this same verdict, so a pinned channel cannot be
    // reopened to unsigned bytes by any pipeline flavor.
    committed_channel_signature_policy(
        workspace_channel_pubkey(repo)?.as_deref(),
        load_signing_material(repo)?
            .as_ref()
            .map(|material| material.pubkey.as_str()),
    )
}

fn sign_manifest_with_policy(ctx: &CutCtx, manifest: &Path) -> Result<PathBuf> {
    let expected_pubkey = ctx
        .signature_pubkey
        .as_deref()
        .ok_or_else(|| Error::new("signature-required cut has no persisted channel public key"))?;
    let material = load_signing_material(&ctx.repo)?.ok_or_else(|| {
        Error::new("signature-required resume needs the recovered offline signing configuration")
    })?;
    if material.pubkey != expected_pubkey {
        return Err(Error::new(
            "current signing key identity differs from the journaled channel public key; \
             refusing key substitution",
        ));
    }
    let signature = manifest.with_extension("toml.sig");
    let out = Command::new(&material.tool)
        .arg("sign")
        .arg(&material.key_path)
        .arg(manifest)
        .arg(&signature)
        .output()
        .map_err(|error| Error::new(format!("spawn {} sign: {error}", material.tool.display())))?;
    if !out.status.success() {
        return Err(Error::new(
            "atpkg-keys sign failed (private key path and tool stderr suppressed)",
        ));
    }
    let manifest_bytes = fs::read(manifest)
        .map_err(|error| Error::new(format!("read {}: {error}", manifest.display())))?;
    let signature_bytes = fs::read(&signature)
        .map_err(|error| Error::new(format!("read {}: {error}", signature.display())))?;
    verify_detached_manifest_signature(expected_pubkey, &manifest_bytes, &signature_bytes)?;
    step(
        "",
        &format!(
            "manifest signed and locally verified (Tier SIG) → {}",
            signature.display()
        ),
    );
    Ok(signature)
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Immutable identity expected from the release under the journal's tag.
#[derive(Debug, Clone, Copy)]
pub struct ExpectedReleaseIdentity<'a> {
    pub version: &'a str,
    pub build: u64,
    pub commit: &'a str,
}

/// Validate the exact bytes about to become archive authority.  This is a
/// pure seam used by normal publish, killed-machine reconstruction, and
/// negative-control tests.  Metadata equality alone is insufficient: the
/// local and live manifest/signature byte strings must match exactly.
pub fn validate_live_release_identity(
    expected: ExpectedReleaseIdentity<'_>,
    live_manifest: &[u8],
    live_signature: Option<&[u8]>,
    local_manifest: Option<&[u8]>,
    local_signature: Option<&[u8]>,
    signature_required: bool,
    signature_pubkey: Option<&str>,
) -> Result<Manifest> {
    if let Some(local) = local_manifest
        && local != live_manifest
    {
        return Err(Error::new(
            "published manifest is not byte-identical to the journaled local artifact",
        ));
    }
    let text = std::str::from_utf8(live_manifest)
        .map_err(|_| Error::new("published manifest is not UTF-8"))?;
    let manifest = Manifest::parse(text)
        .map_err(|error| Error::new(format!("published manifest parse failed: {error}")))?;
    if manifest.version != expected.version
        || manifest.build_number != expected.build
        || manifest.commit.as_deref() != Some(expected.commit)
    {
        return Err(Error::new(format!(
            "published manifest identity is version {:?}, build {}, commit {:?}; expected \
             version {:?}, build {}, commit {}",
            manifest.version,
            manifest.build_number,
            manifest.commit,
            expected.version,
            expected.build,
            expected.commit
        )));
    }
    let expected_dmg = mirror::dmg_asset_name(expected.version);
    if manifest.dmg != expected_dmg {
        return Err(Error::new(format!(
            "published manifest names DMG {:?}, expected exact {expected_dmg:?}",
            manifest.dmg
        )));
    }
    // The zip stays OPTIONAL on the wire (a release cut before zip staging has
    // none), but a manifest that names one must name the canonical one: the
    // client derives this same string from the tag and refuses anything else.
    let expected_zip = mirror::zip_asset_name(expected.version);
    if let Some(zip) = manifest.zip.as_deref()
        && zip != expected_zip
    {
        return Err(Error::new(format!(
            "published manifest names zip {zip:?}, expected exact {expected_zip:?}"
        )));
    }
    match (signature_required, live_signature, signature_pubkey) {
        (true, Some(signature), Some(pubkey)) => {
            if local_signature != Some(signature) {
                return Err(Error::new(
                    "published signature is not byte-identical to the journaled local signature",
                ));
            }
            verify_detached_manifest_signature(pubkey, live_manifest, signature)?;
        }
        (true, None, _) => {
            return Err(Error::new(
                "signature-ratcheted release has no exact published manifest signature",
            ));
        }
        (true, _, None) => {
            return Err(Error::new(
                "signature-ratcheted release has no persisted public-key identity",
            ));
        }
        (false, Some(_), _) => {
            return Err(Error::new(
                "published signature exists but the journal claims an unsigned channel",
            ));
        }
        (false, None, _) => {
            if local_signature.is_some() || signature_pubkey.is_some() {
                return Err(Error::new(
                    "unsigned release carries unexpected local signature/key state",
                ));
            }
        }
    }
    Ok(manifest)
}

fn exact_asset_present(names: &[String], name: &str) -> Result<bool> {
    let count = names
        .iter()
        .filter(|candidate| candidate.as_str() == name)
        .count();
    match count {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(Error::new(format!(
            "release contains {count} assets named {name}; exact identity is ambiguous"
        ))),
    }
}

fn download_live_manifest_pair(
    slug: &str,
    release_id: u64,
    tag: &str,
) -> Result<(Vec<u8>, Option<Vec<u8>>)> {
    let names: Vec<String> = release_asset_inventory_for_release_id(slug, release_id)?
        .into_iter()
        .map(|asset| asset.name)
        .collect();
    if !exact_asset_present(&names, manifest_out::MANIFEST_ASSET)? {
        return Err(Error::new(format!(
            "published release {tag} has no exact {}",
            manifest_out::MANIFEST_ASSET
        )));
    }
    let manifest =
        download_release_asset_for_release_id(slug, release_id, manifest_out::MANIFEST_ASSET)?;
    let signature = if exact_asset_present(&names, manifest_out::MANIFEST_SIG_ASSET)? {
        Some(download_release_asset_for_release_id(
            slug,
            release_id,
            manifest_out::MANIFEST_SIG_ASSET,
        )?)
    } else {
        None
    };
    Ok((manifest, signature))
}

/// The monotonic gate (spec §7 steps 4+5): our claimed `n` must beat the best
/// build the newest-first client scan finds live. Our own tag at exactly `n`
/// is fine — that is this very cut, already (half-)flipped by a crashed
/// earlier attempt the journal is now finishing.
pub fn monotonic_ok(n: u64, our_tag: &str, best: Option<(&str, u64)>) -> Result<()> {
    match best {
        None => Ok(()),
        Some((_, b)) if b < n => Ok(()),
        Some((tag, b)) if b == n && tag == our_tag => Ok(()),
        Some((tag, b)) => Err(Error::new(format!(
            "monotonic check failed: the live client selection rule already finds build \
             {b} ({tag}), not below our {n} — a client would never stage this cut; \
             investigate before publishing"
        ))),
    }
}

// ---------------------------------------------------------------------------
// the cut orchestrator
// ---------------------------------------------------------------------------

/// Everything the pipeline steps share, resolved once up front (or from the
/// journal on `--resume`).
pub struct CutCtx {
    pub repo: PathBuf,
    pub dist: PathBuf,
    pub journal_path: PathBuf,
    /// Publish target ("owner/repo") — origin, or the rehearsal scratch repo.
    pub slug: String,
    pub version: String,
    pub tag: String,
    pub build: u64,
    /// The release commit artifacts must come from (claim commit for a real
    /// cut; HEAD for dry-run/rehearse).
    pub commit: String,
    /// Effective carried channel floor, already validated against `build`.
    pub min_build: Option<u64>,
    pub arm64_only: bool,
    /// Restored from the journal after build; false for legacy journals.
    pub manifest_signed: bool,
    /// Frozen pre-claim channel-signature ratchet and its actual public key.
    pub signature_required: bool,
    pub signature_pubkey: Option<String>,
    /// Immutable GitHub release object ID, persisted in the real-cut journal
    /// as soon as draft creation is observed.
    pub release_id: Option<u64>,
    pub draft_create_issued: bool,
    pub upload_intents: Vec<String>,
    /// The PUBLIC update channel this cut mirrors to, from the tracked
    /// `[workspace.metadata.aterm] update_channel`. `None` = no public mirror
    /// is configured (clients then read [`CutCtx::slug`] directly) and the
    /// `mirror` step is an announced no-op.
    pub mirror_slug: Option<String>,
    pub mirror_release_id: Option<u64>,
    pub mirror_create_issued: bool,
    pub mirror_upload_intents: Vec<String>,
    pub kind: CutKind,
    /// Present only for a real cut while its remote owner ref is held.
    pub lease: Option<ReleaseLeaseGuard>,
    /// Unique per-invocation token; two same-claim resumes cannot share it.
    pub fence: Option<PublisherFenceGuard>,
    /// Which changelog section carries this cut's notes: the rolled
    /// `[version]` for a real cut, `[Unreleased]` for dry-run/rehearse (no
    /// roll ever happens there).
    pub notes_section: String,
    /// Some(..) for a real cut; dry-run/rehearse are deliberately unjournaled
    /// (a provisional n must never look resumable).
    pub journal: Option<Journal>,
}

impl CutCtx {
    fn dmg_path(&self) -> PathBuf {
        self.dist.join(mirror::dmg_asset_name(&self.version))
    }
    /// The updater container (`ditto` zip). Same bundle as the DMG, staged
    /// without `hdiutil` — see `dmg::create_zip`.
    fn zip_path(&self) -> PathBuf {
        self.dist.join(mirror::zip_asset_name(&self.version))
    }
    fn app_path(&self) -> PathBuf {
        self.dist.join("aterm.app")
    }
    fn manifest_path(&self) -> PathBuf {
        self.dist.join(manifest_out::MANIFEST_ASSET)
    }
    fn notes_path(&self) -> PathBuf {
        self.dist.join(format!("notes-{}.md", self.version))
    }
    fn provenance_path(&self) -> PathBuf {
        self.dist.join(format!("aterm-{}-build.txt", self.version))
    }
    fn dsym_zip_path(&self) -> PathBuf {
        self.dist.join(format!("aterm-{}-dSYM.zip", self.version))
    }

    fn is_done(&self, step: &str) -> bool {
        self.journal.as_ref().is_some_and(|j| j.is_done(step))
    }

    fn mark(&mut self, step: &str) -> Result<()> {
        if let Some(j) = &mut self.journal {
            j.mark(step, &self.journal_path)?;
        }
        Ok(())
    }

    fn bind_release_id(&mut self, id: u64) -> Result<()> {
        if id == 0 || self.release_id.is_some_and(|current| current != id) {
            return Err(Error::new(
                "GitHub release ID is zero or differs from the already-bound draft capability",
            ));
        }
        self.release_id = Some(id);
        if let Some(journal) = &mut self.journal {
            if journal.release_id.is_some_and(|current| current != id) {
                return Err(Error::new(
                    "journaled GitHub release ID differs from the observed draft capability",
                ));
            }
            journal.release_id = Some(id);
            journal.save(&self.journal_path)?;
        }
        Ok(())
    }

    pub(crate) fn persist_draft_create_intent(&mut self) -> Result<DurablePostPermit> {
        if self.draft_create_issued {
            return Err(Error::new(
                "draft create intent already exists; refusing to mint another process-local POST permit",
            ));
        }
        if self.kind == CutKind::Real && self.journal.is_none() {
            return Err(Error::new(
                "real draft create has no durable journal; refusing to mint a POST permit",
            ));
        }
        self.draft_create_issued = true;
        if let Some(journal) = &mut self.journal {
            journal.draft_create_issued = true;
            journal.save(&self.journal_path)?;
        }
        Ok(DurablePostPermit(()))
    }

    fn upload_intent_issued(&self, name: &str) -> bool {
        self.upload_intents.iter().any(|issued| issued == name)
    }

    pub(crate) fn persist_upload_intent(&mut self, name: &str) -> Result<DurablePostPermit> {
        if self.upload_intent_issued(name) {
            return Err(Error::new(format!(
                "upload intent for {name} already exists; refusing to mint another process-local POST permit"
            )));
        }
        if self.kind == CutKind::Real && self.journal.is_none() {
            return Err(Error::new(
                "real asset upload has no durable journal; refusing to mint a POST permit",
            ));
        }
        self.upload_intents.push(name.to_string());
        if let Some(journal) = &mut self.journal {
            journal.upload_intents.push(name.to_string());
            journal.save(&self.journal_path)?;
        }
        Ok(DurablePostPermit(()))
    }

    fn required_release_id(&self, operation: &str) -> Result<u64> {
        self.release_id.filter(|id| *id != 0).ok_or_else(|| {
            Error::new(format!(
                "{operation} has no immutable GitHub release ID capability"
            ))
        })
    }

    // --- public-channel mirror capabilities --------------------------------
    // Deliberate twins of the private-side methods above rather than a shared
    // generic: the two repositories must never share an intent set, or a
    // converged upload on one would silently authorize a POST on the other.

    fn bind_mirror_release_id(&mut self, id: u64) -> Result<()> {
        if id == 0 || self.mirror_release_id.is_some_and(|current| current != id) {
            return Err(Error::new(
                "mirror release ID is zero or differs from the already-bound draft capability",
            ));
        }
        self.mirror_release_id = Some(id);
        if let Some(journal) = &mut self.journal {
            if journal
                .mirror_release_id
                .is_some_and(|current| current != id)
            {
                return Err(Error::new(
                    "journaled mirror release ID differs from the observed draft capability",
                ));
            }
            journal.mirror_release_id = Some(id);
            journal.save(&self.journal_path)?;
        }
        Ok(())
    }

    fn persist_mirror_create_intent(&mut self) -> Result<DurablePostPermit> {
        if self.mirror_create_issued {
            return Err(Error::new(
                "mirror create intent already exists; refusing to mint another process-local POST permit",
            ));
        }
        if self.kind == CutKind::Real && self.journal.is_none() {
            return Err(Error::new(
                "real mirror create has no durable journal; refusing to mint a POST permit",
            ));
        }
        self.mirror_create_issued = true;
        if let Some(journal) = &mut self.journal {
            journal.mirror_create_issued = true;
            journal.save(&self.journal_path)?;
        }
        Ok(DurablePostPermit(()))
    }

    fn mirror_upload_intent_issued(&self, name: &str) -> bool {
        self.mirror_upload_intents
            .iter()
            .any(|issued| issued == name)
    }

    fn persist_mirror_upload_intent(&mut self, name: &str) -> Result<DurablePostPermit> {
        if self.mirror_upload_intent_issued(name) {
            return Err(Error::new(format!(
                "mirror upload intent for {name} already exists; refusing to mint another process-local POST permit"
            )));
        }
        if self.kind == CutKind::Real && self.journal.is_none() {
            return Err(Error::new(
                "real mirror upload has no durable journal; refusing to mint a POST permit",
            ));
        }
        self.mirror_upload_intents.push(name.to_string());
        if let Some(journal) = &mut self.journal {
            journal.mirror_upload_intents.push(name.to_string());
            journal.save(&self.journal_path)?;
        }
        Ok(DurablePostPermit(()))
    }

    /// Local paths of exactly the assets that cross to the public channel, in a
    /// stable order. Derived from the same [`mirror::required_asset_names`] the
    /// remote listing is checked against, so the upload set and the acceptance
    /// rule cannot drift apart.
    fn mirror_asset_paths(&self) -> Vec<PathBuf> {
        mirror::required_asset_names(&self.version, self.signature_required)
            .into_iter()
            .map(|name| self.dist.join(name))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteAnnotatedTag {
    token: String,
    commit: String,
}

fn remote_annotated_tag(git: &dyn GitRunner, tag: &str) -> Result<Option<RemoteAnnotatedTag>> {
    let tag_ref = format!("refs/tags/{tag}");
    let peeled_ref = format!("{tag_ref}^{{}}");
    let out = git_ok(
        git,
        &["ls-remote", "--tags", "origin", &tag_ref, &peeled_ref],
    )?;
    let text = out.stdout_utf8();
    let rows: Vec<&str> = text.lines().collect();
    if rows.is_empty() {
        return Ok(None);
    }
    if rows.len() != 2 {
        return Err(Error::new(format!(
            "recovery tag {tag} is not one exact annotated tag plus peel"
        )));
    }
    let mut token = None;
    let mut commit = None;
    for row in rows {
        let mut fields = row.split_whitespace();
        let (Some(oid), Some(reference), None) = (fields.next(), fields.next(), fields.next())
        else {
            return Err(Error::new(format!("malformed remote tag row for {tag}")));
        };
        if !valid_lease_owner(oid) {
            return Err(Error::new(format!("malformed remote tag object for {tag}")));
        }
        if reference == tag_ref {
            token = Some(oid.to_ascii_lowercase());
        } else if reference == peeled_ref {
            commit = Some(oid.to_ascii_lowercase());
        } else {
            return Err(Error::new(format!(
                "remote tag query for {tag} returned unexpected ref {reference}"
            )));
        }
    }
    match (token, commit) {
        (Some(token), Some(commit)) if token != commit => {
            Ok(Some(RemoteAnnotatedTag { token, commit }))
        }
        _ => Err(Error::new(format!(
            "recovery tag {tag} is lightweight or malformed; refusing ambiguous identity"
        ))),
    }
}

/// Bind a published manifest's commit identity to the exact annotated git tag
/// the release advertises. GitHub release metadata alone does not prove that
/// `refs/tags/<tag>` resolves to the signed manifest's claim.
pub fn assert_remote_annotated_tag_commit(
    git: &dyn GitRunner,
    tag: &str,
    expected_commit: &str,
) -> Result<()> {
    let observed = remote_annotated_tag(git, tag)?.ok_or_else(|| {
        Error::new(format!(
            "published release {tag} has no remote annotated tag identity"
        ))
    })?;
    if !observed.commit.eq_ignore_ascii_case(expected_commit) {
        return Err(Error::new(format!(
            "published release tag {tag} peels to {}, not manifest claim {expected_commit}",
            observed.commit
        )));
    }
    Ok(())
}

/// Bind historical published manifests to their exact remote tag refs in one
/// bounded round trip. Current releases are annotated; legacy releases may be
/// lightweight, in which case the direct ref itself must equal the manifest
/// commit. This is deliberately separate from the annotated-only mutation and
/// recovery helpers above.
pub fn assert_remote_historical_tag_commits(
    git: &dyn GitRunner,
    expected: &[(&str, &str)],
) -> Result<()> {
    if expected.is_empty() {
        return Ok(());
    }
    let mut expected_by_tag = std::collections::BTreeMap::new();
    for &(tag, commit) in expected {
        if tag.is_empty() || commit.is_empty() {
            return Err(Error::new(
                "historical tag binding contains an empty tag or commit",
            ));
        }
        if expected_by_tag
            .insert(tag.to_string(), commit.to_string())
            .is_some()
        {
            return Err(Error::new(format!(
                "historical tag binding contains duplicate tag {tag}"
            )));
        }
    }

    let mut query_refs = Vec::with_capacity(expected_by_tag.len() * 2);
    let mut allowed_refs = std::collections::BTreeSet::new();
    for tag in expected_by_tag.keys() {
        let direct = format!("refs/tags/{tag}");
        let peeled = format!("{direct}^{{}}");
        allowed_refs.insert(direct.clone());
        allowed_refs.insert(peeled.clone());
        query_refs.push(direct);
        query_refs.push(peeled);
    }
    let mut args = vec!["ls-remote", "--tags", "origin"];
    args.extend(query_refs.iter().map(String::as_str));
    let out = git_ok(git, &args)?;
    let mut observed = std::collections::BTreeMap::new();
    for row in out.stdout_utf8().lines() {
        let mut fields = row.split_whitespace();
        let (Some(oid), Some(reference), None) = (fields.next(), fields.next(), fields.next())
        else {
            return Err(Error::new("malformed historical remote tag row"));
        };
        if !valid_lease_owner(oid) || !allowed_refs.contains(reference) {
            return Err(Error::new(format!(
                "historical remote tag query returned invalid or unexpected ref {reference}"
            )));
        }
        if observed
            .insert(reference.to_string(), oid.to_ascii_lowercase())
            .is_some()
        {
            return Err(Error::new(format!(
                "historical remote tag query returned duplicate ref {reference}"
            )));
        }
    }

    for (tag, expected_commit) in expected_by_tag {
        let direct_ref = format!("refs/tags/{tag}");
        let peeled_ref = format!("{direct_ref}^{{}}");
        let token = observed.get(&direct_ref).ok_or_else(|| {
            Error::new(format!(
                "published release {tag} has no exact remote tag identity"
            ))
        })?;
        let resolved = match observed.get(&peeled_ref) {
            Some(peeled) if peeled != token => peeled,
            Some(_) => {
                return Err(Error::new(format!(
                    "published release tag {tag} has a malformed annotated identity"
                )));
            }
            None => token,
        };
        if !resolved.eq_ignore_ascii_case(&expected_commit) {
            return Err(Error::new(format!(
                "published release tag {tag} resolves to {resolved}, not manifest claim {expected_commit}"
            )));
        }
    }
    Ok(())
}

/// Delete an exact annotated tag token while an injected safety proof remains
/// true.  The proof runs adjacent to both local and remote mutations; the
/// remote delete additionally uses the observed tag-object CAS, so a recreated
/// tag can never be removed.
pub fn delete_release_tag_with_guard(
    git: &dyn GitRunner,
    tag: &str,
    expected_commit: &str,
    mut before_each_delete: impl FnMut() -> Result<()>,
) -> Result<()> {
    let local = git.git(&["rev-parse", "-q", "--verify", &format!("refs/tags/{tag}")])?;
    let local_token = local
        .success()
        .then(|| local.stdout_utf8().trim().to_ascii_lowercase());
    if let Some(token) = &local_token {
        let kind = git_ok(git, &["cat-file", "-t", token])?;
        let commit = rev_parse(git, &format!("{token}^{{commit}}"))?;
        if kind.stdout_utf8().trim() != "tag" || commit != expected_commit {
            return Err(Error::new(format!(
                "local tag {tag} token {token} is not the expected annotated claim {expected_commit}; refusing delete"
            )));
        }
    }
    let remote = remote_annotated_tag(git, tag)?;
    if let Some(remote) = &remote
        && remote.commit != expected_commit
    {
        return Err(Error::new(format!(
            "remote tag {tag} peels to {}, not recovery claim {expected_commit}; refusing delete",
            remote.commit
        )));
    }
    if let Some(token) = local_token {
        before_each_delete()?;
        git_ok(
            git,
            &["update-ref", "-d", &format!("refs/tags/{tag}"), &token],
        )?;
    }
    let Some(remote) = remote else {
        return Ok(());
    };
    let tag_ref = format!("refs/tags/{tag}");
    let lease = format!("--force-with-lease={tag_ref}:{}", remote.token);
    let delete = format!(":{tag_ref}");
    // The tag token itself may be unchanged across a same-claim recovery.  Its
    // force-with-lease therefore cannot distinguish the killed publisher from
    // the recovery winner: re-prove the unique process token immediately next
    // to the destructive push.
    before_each_delete()?;
    let out = git.git(&["push", &lease, "origin", &delete])?;
    if remote_annotated_tag(git, tag)?.is_some() {
        return Err(Error::new(format!(
            "exact CAS delete of abandoned tag {tag} failed: {}",
            out.stderr_utf8().trim()
        )));
    }
    Ok(())
}

pub fn delete_owned_release_tag(
    git: &dyn GitRunner,
    tag: &str,
    expected_commit: &str,
    lease_guard: &ReleaseLeaseGuard,
    fence_guard: &PublisherFenceGuard,
) -> Result<()> {
    delete_release_tag_with_guard(git, tag, expected_commit, || {
        assert_publisher_session(git, lease_guard, fence_guard)
    })
}

#[must_use]
pub const fn exact_delete_absence_is_converged(
    preexisting_absence_is_converged: bool,
    delete_attempted: bool,
) -> bool {
    preexisting_absence_is_converged || delete_attempted
}

pub fn delete_release_object_by_id_with_guard(
    slug: &str,
    expected: &ReleaseObjectIdentity,
    preexisting_absence_is_converged: bool,
    mut before_identity_recheck: impl FnMut() -> Result<()>,
    mut immediately_before_delete: impl FnMut() -> Result<()>,
) -> Result<bool> {
    let mut last = String::new();
    let mut delete_attempted = false;
    for (attempt, backoff) in [(1u32, 2u64), (2, 5), (3, 0)] {
        let Some(observed) = release_object_by_id(slug, expected.id)? else {
            if exact_delete_absence_is_converged(preexisting_absence_is_converged, delete_attempted)
            {
                return Ok(false);
            }
            return Err(Error::new(format!(
                "exact release ID {} became absent before this guarded invocation issued DELETE; refusing transient absence as cleanup authority",
                expected.id
            )));
        };
        validate_release_object_snapshot(Some(&observed), expected)?;
        before_identity_recheck()?;
        let adjacent = release_object_by_id(slug, expected.id)?;
        validate_release_object_snapshot(adjacent.as_ref(), expected)?;
        // Cross-system state cannot be atomically transacted with GitHub's
        // DELETE. Keep the cheap unique publisher-token check last; the exact
        // object capability was re-read immediately before it.
        immediately_before_delete()?;
        let endpoint = format!("repos/{slug}/releases/{}", expected.id);
        let out = gh_raw(&["api", "--method", "DELETE", &endpoint])?;
        delete_attempted = true;
        if release_object_by_id(slug, expected.id)?.is_none() {
            return Ok(true);
        }
        last = out.stderr_utf8().trim().to_string();
        if attempt < 3 {
            eprintln!(
                "    exact release-ID delete failed (attempt {attempt}/3): {last} — retrying in {backoff}s"
            );
            std::thread::sleep(std::time::Duration::from_secs(backoff));
        }
    }
    Err(Error::new(format!(
        "delete exact GitHub release ID {} failed after 3 attempts: {last}",
        expected.id
    )))
}

/// Delete an unpublished draft only while the exact owner+process token is
/// still current, and only when GitHub says the draft targets that owner.
/// Published state is never inside this helper's authority.
pub fn delete_owned_draft_release(
    repo: &Path,
    slug: &str,
    tag: &str,
    expected_release_id: Option<u64>,
    create_intent_knowledge: Option<bool>,
    lease: &ReleaseLeaseGuard,
    fence: &PublisherFenceGuard,
) -> Result<bool> {
    let git = GitCli::new(repo);
    assert_publisher_session(&git, lease, fence)?;
    let by_tag = unique_release_object_by_tag(slug, tag)?;
    match draft_cleanup_decision(create_intent_knowledge, by_tag.is_some()) {
        DraftCleanupDecision::AbandonProvenNoPost => return Ok(false),
        DraftCleanupDecision::DeleteIssuedVisible => {}
        DraftCleanupDecision::RetainIssuedAwaitVisibility => {
            return Err(Error::new(format!(
                "draft-create intent for {tag} was issued but no exact object is visible; retaining owner/journal until delayed visibility converges"
            )));
        }
        DraftCleanupDecision::RefuseUnknownOrInconsistent => {
            return Err(Error::new(format!(
                "draft cleanup knowledge/visibility is unknown or inconsistent for {tag}; retaining owner/journal"
            )));
        }
    }
    let by_tag = by_tag.expect("visible cleanup decision");
    let release = if let Some(expected_id) = expected_release_id {
        let Some(release) = release_object_by_id(slug, expected_id)? else {
            return Err(Error::new(format!(
                "journaled draft release ID {expected_id} is absent before a durable delete-start receipt; retaining owner/journal rather than treating a transient 404 as cleanup convergence"
            )));
        };
        validate_release_object_capability(Some(&release), expected_id, tag, lease.owner(), true)?;
        if by_tag.id != expected_id {
            return Err(Error::new(format!(
                "exact tag {tag} resolves to replacement release ID {}, not journal capability {expected_id}",
                by_tag.id
            )));
        }
        release
    } else {
        by_tag
    };
    if !release.draft {
        return Err(Error::new(format!(
            "{tag} release ID {} is PUBLISHED; refusing draft deletion",
            release.id
        )));
    }
    validate_release_object_capability(Some(&release), release.id, tag, lease.owner(), true)?;
    let deleted = delete_release_object_by_id_with_guard(
        slug,
        &release,
        false,
        || Ok(()),
        || assert_publisher_session(&git, lease, fence),
    )?;
    if !release_objects_by_tag(slug, tag)?.is_empty() {
        return Err(Error::new(format!(
            "draft release ID {} was deleted but {tag} now resolves to a replacement; refusing tag/lease cleanup",
            release.id
        )));
    }
    Ok(deleted)
}

fn recovery_claim_build(git: &dyn GitRunner, version: &str, owner: &str) -> Result<u64> {
    // Fetch through the advertised owner ref; servers commonly forbid fetches
    // by arbitrary unadvertised SHA on a replacement machine.
    git_ok(git, &["fetch", "--no-tags", "origin", RELEASE_LEASE_REF])?;
    let object = git.git(&["cat-file", "-e", &format!("{owner}^{{commit}}")])?;
    if !object.success() {
        return Err(Error::new(format!(
            "release lease owner {owner} is not an available commit object"
        )));
    }
    let shown = git_ok(git, &["show", &format!("{owner}:{}", ledger::LEDGER_FILE)])?;
    let ledger_text = String::from_utf8(shown.stdout)
        .map_err(|_| Error::new("claim commit ledger is not UTF-8"))?;
    let tail = ledger::tail(&ledger_text)?;
    if tail.version != version {
        return Err(Error::new(format!(
            "claim commit {owner} ledger tail is build {} version {}, not requested v{version}",
            tail.build, tail.version
        )));
    }
    Ok(tail.build)
}

fn recovery_worktree_preflight(git: &dyn GitRunner) -> Result<()> {
    gates::clean_tree(git)?;
    let branch = git_ok(git, &["symbolic-ref", "--short", "HEAD"])?
        .stdout_utf8()
        .trim()
        .to_string();
    if branch != "main" {
        return Err(Error::new(format!(
            "lost-machine recovery must run on main, not {branch:?}"
        )));
    }
    git_ok(git, &["fetch", "origin", "main"])?;
    let head = rev_parse(git, "HEAD")?;
    let remote = rev_parse(git, "origin/main")?;
    if head != remote {
        return Err(Error::new(format!(
            "lost-machine recovery requires HEAD == origin/main ({head} != {remote}); pull first"
        )));
    }
    Ok(())
}

/// Resume requires a clean tree, with no exceptions.
///
/// Format 6 and earlier carried one: the `cask` step wrote and staged a derived
/// pin into the shared checkout before committing it, so a crash in that window
/// left a legitimately dirty tree that resume had to admit byte-for-byte. That
/// step is gone (format 7), and no current step mutates the checkout before
/// committing, so the exception has no state left to admit. It is not merely
/// unused: an unfinished v6 journal cannot reach here at all, because
/// [`Journal::ensure_resumable`] refuses any unfinished journal below
/// [`JOURNAL_FORMAT`] and routes it to stopped-publisher recovery.
pub fn recovery_resume_worktree_preflight(
    _repo: &Path,
    git: &dyn GitRunner,
    _journal: &Journal,
) -> Result<()> {
    gates::clean_tree(git)
}

/// Bind an ordinary `--resume` to the immutable claim before the pipeline can
/// reacquire either publication ref.  The journal is only a crash cursor: it
/// is never authority for `(version, build, commit)`, and a structurally valid
/// file edited by hand must not be able to steer a late upload/flip.
///
/// This preflight deliberately performs the worktree check first, so an
/// unrelated staged/unstaged/untracked path is rejected before even the
/// read-only fetch. No dirty state is admitted (see
/// [`recovery_resume_worktree_preflight`]).
pub fn ordinary_resume_claim_preflight(
    repo: &Path,
    git: &dyn GitRunner,
    journal: &Journal,
) -> Result<()> {
    recovery_resume_worktree_preflight(repo, git, journal)?;
    gates::on_main(git)?;

    git_ok(git, &["fetch", "origin", "main"])
        .map_err(|error| Error::new(format!("cannot refresh origin/main for resume: {error}")))?;
    let object = git.git(&["cat-file", "-e", &format!("{}^{{commit}}", journal.commit)])?;
    if !object.success() {
        return Err(Error::new(format!(
            "journal claim {} is not an available commit object",
            journal.commit
        )));
    }
    let shown = git_ok(
        git,
        &[
            "show",
            &format!("{}:{}", journal.commit, ledger::LEDGER_FILE),
        ],
    )?;
    let ledger_text = String::from_utf8(shown.stdout)
        .map_err(|_| Error::new("journal claim ledger is not UTF-8"))?;
    let tail = ledger::tail(&ledger_text)?;
    if tail.version != journal.version || tail.build != journal.build_number {
        return Err(Error::new(format!(
            "journal identity v{} build {} is not the exact claim-commit ledger tail v{} build {}",
            journal.version, journal.build_number, tail.version, tail.build
        )));
    }
    let ancestor = git.git(&[
        "merge-base",
        "--is-ancestor",
        &journal.commit,
        "origin/main",
    ])?;
    if !ancestor.success() {
        return Err(Error::new(format!(
            "journal claim {} is not an ancestor of origin/main; refusing a stale or foreign resume",
            journal.commit
        )));
    }
    if !journal.is_done("build") {
        let head = rev_parse(git, "HEAD")?;
        if head != journal.commit {
            return Err(Error::new(format!(
                "HEAD ({head}) is not the journaled claim commit ({}) — check it out \
                 (or run a plain `cargo ship cut` to recut with a fresh number)",
                journal.commit
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_claim_provenance(
    bytes: &[u8],
    version: &str,
    build: u64,
    owner: &str,
) -> Result<()> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| Error::new("release provenance is not UTF-8"))?;
    let field = |name: &str| -> Result<&str> {
        let prefix = format!("{name}=");
        let mut values = text.lines().filter_map(|line| line.strip_prefix(&prefix));
        let first = values
            .next()
            .ok_or_else(|| Error::new(format!("release provenance has no exact {name}= field")))?;
        if values.next().is_some() {
            return Err(Error::new(format!(
                "release provenance duplicates {name}= identity"
            )));
        }
        Ok(first)
    };
    let owner_short = owner
        .get(..12)
        .ok_or_else(|| Error::new("release claim is too short for provenance identity"))?;
    if field("version")? != version
        || field("build")? != build.to_string()
        || field("commit")? != owner_short
    {
        return Err(Error::new(
            "release provenance version/build/short-commit does not match the claim",
        ));
    }
    Ok(())
}

fn combine_with_fence_release(
    result: Result<()>,
    git: &dyn GitRunner,
    fence: &PublisherFenceGuard,
) -> Result<()> {
    let release = release_publisher_fence(git, fence).map(|_| ());
    match (result, release) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(fence_error)) => Err(Error::new(format!(
            "recovery completed but publisher-fence cleanup failed: {fence_error}"
        ))),
        (Err(error), Err(fence_error)) => Err(Error::new(format!(
            "{error}; publisher-fence cleanup also failed: {fence_error}"
        ))),
    }
}

/// Explicit cross-machine recovery for a persistent lease whose local journal
/// was lost.  Draft/absent cuts are safely abandoned; an already-published
/// exact-identity cut is reconstructed at `archive` and finished through
/// verification, and unlock.  A published release is never deleted here. The
/// boolean is the caller/operator's explicit stopped-process assertion, not a
/// machine proof; false refuses before reading repository or remote state.
pub fn run_recover_lost(
    repo: &Path,
    version: &str,
    owner: &str,
    old_process_stopped: bool,
) -> Result<()> {
    if !old_process_stopped {
        return Err(Error::new(RECOVERY_STOPPED_PROCESS_REFUSAL));
    }
    ledger::check_version_shape(version)?;
    if !valid_lease_owner(owner) {
        return Err(Error::new(
            "recover requires the full 40- or 64-hex claim commit printed by the lease",
        ));
    }
    let owner = owner.to_ascii_lowercase();
    let cargo_text = fs::read_to_string(repo.join("Cargo.toml"))
        .map_err(|error| Error::new(format!("read Cargo.toml: {error}")))?;
    let slug = repo_slug(&cargo_text)
        .ok_or_else(|| Error::new("Cargo.toml repository is not an exact GitHub OWNER/REPO URL"))?;
    let git = GitCli::new(repo);
    assert_origin_repo_binding(&git, &slug)?;
    let journal_path = repo.join("dist/cut-state.toml");
    let journal = Journal::load(&journal_path)?;
    if let Some(journal) = &journal {
        journal.ensure_resumable()?;
        if journal.version != version || !journal.commit.eq_ignore_ascii_case(&owner) {
            return Err(Error::new(format!(
                "local journal is v{} owner {}, not requested recovery v{version} {owner}",
                journal.version, journal.commit
            )));
        }
    }
    if release_lease_owner(&git)?.as_deref() != Some(owner.as_str()) {
        return Err(Error::new(format!(
            "persistent release lease is not owned by supplied claim {owner}; refusing recovery"
        )));
    }
    let build = recovery_claim_build(&git, version, &owner)?;
    if let Some(journal) = &journal
        && journal.build_number != build
    {
        return Err(Error::new(format!(
            "local journal build {} differs from claim ledger tail {build}",
            journal.build_number
        )));
    }
    if let Some(journal) = &journal {
        recovery_resume_worktree_preflight(repo, &git, journal)?;
        gates::on_main(&git)?;
        git_ok(&git, &["fetch", "origin", "main"])?;
        if !journal.is_done("build") {
            let head = rev_parse(&git, "HEAD")?;
            if head != owner {
                return Err(Error::new(format!(
                    "recovery must rebuild from journal claim {owner}, but HEAD is {head}"
                )));
            }
        }
    } else {
        recovery_worktree_preflight(&git)?;
    }
    let ancestor = git.git(&["merge-base", "--is-ancestor", &owner, "origin/main"])?;
    if !ancestor.success() {
        return Err(Error::new(format!(
            "recovery claim {owner} is not an ancestor of origin/main; refusing an unbound lease"
        )));
    }

    // The release-state probe stays ahead of the fence rotation: an
    // unreachable remote must fail recovery before its first mutation.
    verify::release_state(&slug, &format!("v{version}"))?;
    // Validate the immutable signing identity before rotating a killed
    // process's token.  Missing key recovery therefore leaves the old fence
    // untouched and the channel visibly blocked, never silently unsigned.
    let signature_policy = preflight_signature_policy(repo)?;
    if let Some(journal) = &journal
        && (journal.signature_required != signature_policy.required
            || journal.signature_pubkey.as_deref() != signature_policy.pubkey.as_deref())
    {
        return Err(Error::new(
            "recovery journal signing policy/key differs from the current signing configuration",
        ));
    }

    // This is the last line before the first recovery mutation. The flag is an
    // explicit operator assertion; no local program can prove a process on a
    // lost machine is quiescent or cancel its already-issued REST request.
    step("recover", RECOVERY_STOPPED_PROCESS_BANNER);
    let fence = rotate_publisher_fence_for_recovery(&git, &owner)?;
    let resume_local_journal = journal.is_some();
    let create_intent_knowledge = journal.as_ref().and_then(|journal| {
        (journal.format == JOURNAL_FORMAT).then_some(journal.draft_create_issued)
    });
    let expected_release_id = journal.as_ref().and_then(|journal| journal.release_id);
    let abandoned_journal =
        (journal.is_some() && !resume_local_journal).then_some(journal_path.as_path());
    let result = if let Some(journal) = journal
        && resume_local_journal
    {
        confirm_release_lease_owner(&git, &owner).and_then(|lease| {
            resume_cut(
                repo,
                &repo.join("dist"),
                &journal_path,
                &slug,
                journal,
                Instant::now(),
                Some((lease, fence.clone())),
            )
        })
    } else {
        recover_under_fence(
            repo,
            &slug,
            LostRecoveryPlan {
                version,
                build,
                owner: &owner,
                create_intent_knowledge,
                expected_release_id,
                abandoned_journal,
            },
            &fence,
        )
    };
    combine_with_fence_release(result, &git, &fence)
}

struct LostRecoveryPlan<'a> {
    version: &'a str,
    build: u64,
    owner: &'a str,
    create_intent_knowledge: Option<bool>,
    expected_release_id: Option<u64>,
    abandoned_journal: Option<&'a Path>,
}

fn recover_under_fence(
    repo: &Path,
    slug: &str,
    plan: LostRecoveryPlan<'_>,
    fence: &PublisherFenceGuard,
) -> Result<()> {
    let LostRecoveryPlan {
        version,
        build,
        owner,
        create_intent_knowledge,
        expected_release_id,
        abandoned_journal,
    } = plan;
    let git = GitCli::new(repo);
    let lease = confirm_release_lease_owner(&git, owner)?;
    assert_publisher_session(&git, &lease, fence)?;
    let tag = format!("v{version}");
    match verify::release_state(slug, &tag)? {
        verify::ReleaseState::Published => {
            let fresh_policy = fresh_published_recovery_signature_policy(repo, slug, version)?;
            recover_published_cut(
                repo,
                slug,
                version,
                build,
                owner,
                &fresh_policy,
                lease,
                fence.clone(),
            )
        }
        verify::ReleaseState::Draft | verify::ReleaseState::Absent => {
            // The explicit recover command is the operator's assertion that
            // the killed publisher is stopped.  Cooperative contenders are
            // excluded by our fresh exact token; recheck immediately before
            // each destructive operation.
            assert_publisher_session(&git, &lease, fence)?;
            match verify::release_state(slug, &tag)? {
                verify::ReleaseState::Draft => {
                    if !delete_owned_draft_release(
                        repo,
                        slug,
                        &tag,
                        expected_release_id,
                        create_intent_knowledge,
                        &lease,
                        fence,
                    )? {
                        return Err(Error::new(format!(
                            "draft {tag} was not deleted under exact one-shot recovery authority"
                        )));
                    }
                    step("recover", &format!("unpublished exact draft {tag} deleted"));
                }
                verify::ReleaseState::Absent => {
                    if absent_draft_decision(create_intent_knowledge)
                        == AbsentDraftDecision::RetainOwnerAwaitVisibility
                    {
                        return Err(Error::new(format!(
                            "release {tag} is currently absent, but draft-create intent is {}. \
                             An accepted POST may still become visible; retaining the claim lease \
                             and refusing tag/journal cleanup until the exact draft converges",
                            if create_intent_knowledge == Some(true) {
                                "known issued"
                            } else {
                                "unknown because the current journal is unavailable"
                            }
                        )));
                    }
                }
                verify::ReleaseState::Published => {
                    let fresh_policy =
                        fresh_published_recovery_signature_policy(repo, slug, version)?;
                    return recover_published_cut(
                        repo,
                        slug,
                        version,
                        build,
                        owner,
                        &fresh_policy,
                        lease,
                        fence.clone(),
                    );
                }
            }
            assert_publisher_session(&git, &lease, fence)?;
            delete_owned_release_tag(&git, &tag, owner, &lease, fence)?;
            release_completed_publisher_session(&git, owner, fence)?;
            if let Some(journal_path) = abandoned_journal {
                match fs::remove_file(journal_path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(Error::new(format!(
                            "unpublished historical recovery released its remote owner but could not remove {}: {error}",
                            journal_path.display()
                        )));
                    }
                }
            }
            step(
                "recover",
                "unpublished cut safely abandoned · exact owner lease released",
            );
            Ok(())
        }
    }
}

fn fresh_published_recovery_signature_policy(
    repo: &Path,
    slug: &str,
    version: &str,
) -> Result<SignaturePolicy> {
    let state = verify::release_state(slug, &format!("v{version}"))?;
    if state != verify::ReleaseState::Published {
        return Err(Error::new(format!(
            "recovery release v{version} changed away from Published while refreshing its signature authority"
        )));
    }
    preflight_signature_policy(repo)
}

#[allow(clippy::too_many_arguments)]
fn recover_published_cut(
    repo: &Path,
    slug: &str,
    version: &str,
    build: u64,
    owner: &str,
    signature_policy: &SignaturePolicy,
    lease: ReleaseLeaseGuard,
    fence: PublisherFenceGuard,
) -> Result<()> {
    let git = GitCli::new(repo);
    assert_publisher_session(&git, &lease, &fence)?;
    let tag = format!("v{version}");
    let remote_tag = remote_annotated_tag(&git, &tag)?.ok_or_else(|| {
        Error::new(format!(
            "published recovery release {tag} has no remote annotated tag"
        ))
    })?;
    if remote_tag.commit != owner {
        return Err(Error::new(format!(
            "published recovery tag {tag} peels to {}, not claim {owner}",
            remote_tag.commit
        )));
    }
    let release_object = unique_release_object_by_tag(slug, &tag)?.ok_or_else(|| {
        Error::new(format!(
            "published recovery release {tag} vanished while binding its immutable ID"
        ))
    })?;
    validate_release_object_capability(
        Some(&release_object),
        release_object.id,
        &tag,
        owner,
        false,
    )?;
    let (manifest_bytes, signature_bytes) =
        download_live_manifest_pair(slug, release_object.id, &tag)?;
    let manifest = validate_live_release_identity(
        ExpectedReleaseIdentity {
            version,
            build,
            commit: owner,
        },
        &manifest_bytes,
        signature_bytes.as_deref(),
        None,
        signature_bytes.as_deref(),
        signature_policy.required,
        signature_policy.pubkey.as_deref(),
    )?;
    let names: Vec<String> = release_asset_inventory_for_release_id(slug, release_object.id)?
        .into_iter()
        .map(|asset| asset.name)
        .collect();
    if !exact_asset_present(&names, &manifest.dmg)? {
        return Err(Error::new(format!(
            "published recovery release has no exact DMG {}",
            manifest.dmg
        )));
    }
    // The updater container must be recoverable too: the mirror step serves the
    // public channel from the reconstructed dist/, and the required asset set
    // includes the zip.
    let (recovered_zip, recovered_zip_sha256) =
        match (manifest.zip.as_deref(), manifest.zip_sha256.as_deref()) {
            (Some(zip), Some(sha256)) => (zip.to_string(), sha256.to_string()),
            _ => {
                return Err(Error::new(
                    "published recovery release carries no zip name + digest pair; it predates \
                     zip staging and cannot be recovered by this cutter — finish or retire it \
                     by hand",
                ));
            }
        };
    if !exact_asset_present(&names, &recovered_zip)? {
        return Err(Error::new(format!(
            "published recovery release has no exact zip {recovered_zip}"
        )));
    }
    let provenance_name = format!("aterm-{version}-build.txt");
    if !exact_asset_present(&names, &provenance_name)? {
        return Err(Error::new(format!(
            "published recovery release has no exact provenance asset {provenance_name}; \
             the current archive/verify suffix requires its version/build/commit proof"
        )));
    }
    let provenance =
        download_release_asset_for_release_id(slug, release_object.id, &provenance_name)?;
    validate_claim_provenance(&provenance, version, build, owner)?;

    // Reconstruct only authoritative, remotely validated bytes.  The journal
    // begins after flip: build/upload are never replayed from guesses, while
    // archive/verify remain convergent production steps.
    let dist = repo.join("dist");
    fs::create_dir_all(&dist)
        .map_err(|error| Error::new(format!("create {}: {error}", dist.display())))?;
    verify_release_asset_digest_for_release_id_to(
        slug,
        release_object.id,
        &tag,
        &manifest.dmg,
        &manifest.sha256,
        &dist.join(&manifest.dmg),
    )?;
    verify_release_asset_digest_for_release_id_to(
        slug,
        release_object.id,
        &tag,
        &recovered_zip,
        &recovered_zip_sha256,
        &dist.join(&recovered_zip),
    )?;
    fs::write(dist.join(manifest_out::MANIFEST_ASSET), &manifest_bytes)
        .map_err(|error| Error::new(format!("reconstruct manifest: {error}")))?;
    if let Some(signature) = &signature_bytes {
        fs::write(dist.join(manifest_out::MANIFEST_SIG_ASSET), signature)
            .map_err(|error| Error::new(format!("reconstruct signature: {error}")))?;
    } else {
        match fs::remove_file(dist.join(manifest_out::MANIFEST_SIG_ASSET)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(Error::new(format!(
                    "remove stale recovered manifest signature: {error}"
                )));
            }
        }
    }
    fs::write(dist.join(&provenance_name), provenance)
        .map_err(|error| Error::new(format!("reconstruct provenance: {error}")))?;
    let journal_path = dist.join("cut-state.toml");
    let journal = Journal {
        format: JOURNAL_FORMAT,
        version: version.to_string(),
        build_number: build,
        commit: owner.to_string(),
        min_build: manifest.min_build,
        arm64_only: false,
        manifest_signed: signature_policy.required,
        signature_required: signature_policy.required,
        signature_pubkey: signature_policy.pubkey.clone(),
        release_id: Some(release_object.id),
        draft_create_issued: true,
        upload_intents: Vec::new(),
        // A recovered cut has no mirror capability yet: the mirror step runs
        // after `verify`, which this reconstruction has not reached, so it
        // starts from a clean one-shot intent set.
        mirror_release_id: None,
        mirror_create_issued: false,
        mirror_upload_intents: Vec::new(),
        done: [
            "lock",
            "build",
            "selfcheck",
            "draft",
            "upload",
            "preflip",
            "tag",
            "flip",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
    };
    journal.save(&journal_path)?;
    step(
        "recover",
        &format!(
            "validated published {tag} version/build/commit + manifest{} + DMG digest{}; \
             reconstructed journal at archive",
            if signature_policy.required {
                "/signature/public-key"
            } else {
                ""
            },
            " + provenance"
        ),
    );
    let mut ctx = CutCtx {
        repo: repo.to_path_buf(),
        dist,
        journal_path,
        slug: slug.to_string(),
        version: version.to_string(),
        tag,
        build,
        commit: owner.to_string(),
        min_build: manifest.min_build,
        arm64_only: false,
        manifest_signed: signature_policy.required,
        signature_required: signature_policy.required,
        signature_pubkey: signature_policy.pubkey.clone(),
        release_id: Some(release_object.id),
        draft_create_issued: true,
        upload_intents: Vec::new(),
        mirror_slug: workspace_mirror_slug(repo)?,
        mirror_release_id: None,
        mirror_create_issued: false,
        mirror_upload_intents: Vec::new(),
        kind: CutKind::Real,
        lease: Some(lease),
        fence: Some(fence),
        notes_section: version.to_string(),
        journal: Some(journal),
    };
    run_pipeline(&mut ctx, Instant::now())
}

/// The whole `cargo ship cut` (spec §7 order): gates → claim → build+package
/// → self-check → draft-first publish → post-publish verify.
///
/// The version comes from `[workspace.package] version` with the DEV
/// component reset to 0 ([`release_version_from_workspace`]) — NOT from the
/// ledger, which supplies only the build number. Cutting twice without
/// bumping Cargo.toml therefore lands on the already-published guard in
/// [`verify::derive_cut_mode`], which names the bump.
pub fn run_cut(repo: &Path, opts: &CutOptions) -> Result<()> {
    let t0 = Instant::now();
    let dist = repo.join("dist");
    let journal_path = dist.join("cut-state.toml");
    let git = GitCli::new(repo);

    let cargo_text = fs::read_to_string(repo.join("Cargo.toml"))
        .map_err(|e| Error::new(format!("read Cargo.toml: {e}")))?;
    let full = workspace_version(&cargo_text)?;
    let origin_slug = repo_slug(&cargo_text).ok_or_else(|| {
        Error::new(
            "Cargo.toml [workspace.package] repository is not an exact GitHub OWNER/REPO URL",
        )
    })?;
    // The PUBLIC channel installed copies read. Parsed from the same tracked
    // key `aterm-update-core/build.rs` compiles into every client, so the
    // pipeline mirrors to exactly the place the fleet looks.
    let mirror_slug = mirror::update_channel_slug(&cargo_text)?;
    // THE version this cut publishes: the workspace version with DEV reset to
    // 0. The ledger is still read (below) for the BUILD NUMBER claim, but it
    // is no longer a version lineage — its historical two-component lines are
    // retired-scheme accounting history.
    let release_version = release_version_from_workspace(&full)?;

    let kind = if opts.dry_run {
        CutKind::DryRun
    } else if opts.rehearse.is_some() {
        CutKind::Rehearse
    } else {
        CutKind::Real
    };
    let publish_slug = opts.rehearse.clone().unwrap_or_else(|| origin_slug.clone());
    if kind == CutKind::Real {
        assert_origin_repo_binding(&git, &origin_slug)?;
    }

    // ---- journal triage (before anything else) ----------------------------
    let existing = Journal::load(&journal_path)?;
    if opts.resume {
        let j = existing.ok_or_else(|| {
            Error::new(
                "nothing to resume — no dist/cut-state.toml. A wedged cut from another \
                 machine is recovered by a plain `cargo ship cut` (remote-derived recut)."
                    .to_string(),
            )
        })?;
        if kind != CutKind::Real {
            return Err(Error::new(
                "--resume applies to a real cut only (dry-run/rehearse are never journaled)"
                    .to_string(),
            ));
        }
        return resume_cut(repo, &dist, &journal_path, &origin_slug, j, t0, None);
    }
    if let Some(j) = &existing {
        match j.first_incomplete() {
            // A finished cut's journal is just history — clear it and move on.
            None => {
                let _ = fs::remove_file(&journal_path);
            }
            Some(next) if kind == CutKind::Real => {
                return Err(Error::new(format!(
                    "a cut is already in progress: v{} (build {}) is journaled at step \
                     \"{next}\" — finish it with `cargo ship cut --resume`, discard it \
                     with `cargo ship cut --abandon v{}`, or delete dist/cut-state.toml",
                    j.version, j.build_number, j.version
                )));
            }
            Some(next) => {
                // Dry-run/rehearse never touch the journal itself — but they
                // rebuild dist/ IN PLACE under a provisional number, into the
                // very paths the journaled cut's remaining steps will upload.
                // A later --resume would then ship a MIXED asset set (the real
                // cut's DMG next to a provisional-number manifest) and flip a
                // self-inconsistent release live. Refuse while a real cut is
                // in flight.
                return Err(Error::new(format!(
                    "an unfinished real cut is journaled: v{} (build {}) at step \
                     \"{next}\" — a {} would overwrite its dist/ artifacts with \
                     provisional-number ones; finish it (`cargo ship cut --resume`) \
                     or discard it (`cargo ship cut --abandon v{}`) first",
                    j.version,
                    j.build_number,
                    if kind == CutKind::DryRun {
                        "dry-run"
                    } else {
                        "rehearsal"
                    },
                    j.version
                )));
            }
        }
    }

    // ---- decide the version (fresh vs remote-derived recut, spec §5) ------
    let changelog_text = fs::read_to_string(repo.join(changelog::CHANGELOG_FILE))
        .map_err(|e| Error::new(format!("read {}: {e}", changelog::CHANGELOG_FILE)))?;
    let (version, recut) = if kind == CutKind::Real {
        let has_section = changelog::has_section(&changelog_text, &release_version);
        let published = if has_section {
            // Only hit the network when the wedge signature is plausible.
            verify::release_state(&origin_slug, &format!("v{release_version}"))?
                == verify::ReleaseState::Published
        } else {
            false
        };
        let state = verify::RemoteState {
            current_version: release_version.clone(),
            changelog_has_section: has_section,
            published,
        };
        match verify::derive_cut_mode(&state, opts.set_version.as_deref())? {
            verify::CutMode::Fresh { version } => (version, false),
            verify::CutMode::Recut { version } => (version, true),
        }
    } else {
        // Dry-run/rehearse never roll, so there is no recut concept: version
        // is the explicit override or the workspace-derived release version;
        // notes come from [Unreleased].
        match &opts.set_version {
            Some(v) => (v.clone(), false),
            None => (release_version.clone(), false),
        }
    };
    ledger::check_version_shape(&version)?;

    let head8: String = rev_parse(&git, "HEAD")?.chars().take(8).collect();
    let flavor = match kind {
        CutKind::Real if recut => " [recut]",
        CutKind::Real => "",
        CutKind::DryRun => " [dry-run]",
        CutKind::Rehearse => " [rehearse]",
    };
    println!("aterm-release · cut v{version} (workspace {full}, main @ {head8}){flavor}");

    // ---- gates (spec §6; <5s, before anything is committed) ---------------
    let gate_opts = gates::GateOpts {
        version: version.clone(),
        arm64_only: opts.arm64_only,
        recut,
        // Only a REAL cut is compared against the public channel: a dry run
        // uploads nothing and a rehearsal uploads to a scratch repo, so in
        // neither case can the channel be expected to carry this version. This
        // is the sole opt-out and it is structural — derived from the flags,
        // never readable from the environment.
        offline: !matches!(kind, CutKind::Real),
    };
    let gr = gates::run_all(&git, repo, &gate_opts)?;
    step(
        "gates",
        &format!(
            "clean tree on main · HEAD == origin/main ({}) · tag v{version} free (local+remote)",
            gr.head_short
        ),
    );
    step(
        "",
        &format!(
            "CHANGELOG [{}]: {} entries, no ''' · gh auth ({})",
            if recut {
                version.as_str()
            } else {
                "Unreleased"
            },
            gr.changelog_entries,
            gr.gh_account.as_deref().unwrap_or("account unknown"),
        ),
    );
    step(
        "",
        &format!(
            "Cargo.lock exact/offline · trustc ok ({}) · {} · disk ok ({} GiB free)",
            gr.trustc.display(),
            if gr.universal {
                "x86_64 target ok"
            } else {
                "arm64-only"
            },
            gr.free_disk_gib,
        ),
    );
    step(
        "",
        &match gr.channel_version.as_deref() {
            Some(v) => format!("public channel source agrees: carries {v}"),
            None => "public channel source version: not checked (no channel/manifest)".to_string(),
        },
    );
    if opts.gate {
        run_gate_script(repo)?;
    }

    if kind == CutKind::Real {
        preflight_release_lease(&git)?;
        step("lease", "remote release lease is free (pre-claim)");
        // Prove the public channel is writable BEFORE the ledger claim. The
        // mirror is the last remote step; failing it after the claim would burn
        // a build number and leave a live release the fleet cannot see, and no
        // amount of `--resume` fixes a missing permission grant.
        match &mirror_slug {
            Some(slug) if *slug != origin_slug => {
                // Prove the CHANNEL credential, not `gh auth`: the mirror step will
                // authenticate with the release-org token, so a preflight on the dev
                // account would refuse a cut that would actually have succeeded.
                let _cred = ChannelCred::enter();
                preflight_mirror_target(slug)?;
                step(
                    "mirror",
                    &format!("public update channel {slug} is public and writable (pre-claim)"),
                );
            }
            Some(_) => {}
            None => {
                step(
                    "mirror",
                    &format!(
                        "no {} {} declared — shipped builds will read {origin_slug}, which \
                         needs a per-machine token",
                        mirror::CHANNEL_TABLE,
                        mirror::CHANNEL_KEY
                    ),
                );
            }
        }
    }

    // ---- channel floor (before claim: bad input must not burn a number) ----
    // The updater selects the first valid manifest on GitHub's newest-first
    // release stream. Its floor is channel state, not a one-cut CLI option:
    // every successor must carry it forward or a fresh client could forget a
    // prior yank. The late selfcheck/preflip/flip scans repeat this guard to
    // close the race with another publisher.
    let channel = if kind == CutKind::Rehearse {
        verify::scan_published(&publish_slug, true)?
    } else {
        verify::scan_published_in_repo(repo, &publish_slug, true)?
    };
    let newest_channel = channel.first();
    let newest_min_build = newest_channel.and_then(|published| published.min_build);
    let signature_policy = preflight_signature_policy(repo)?;
    step(
        "signature",
        &match (
            workspace_channel_pubkey(repo)?,
            signature_policy.required,
        ) {
            (Some(pin), _) => format!(
                "committed channel anchor (aterm-update-core::pins) pins signing to \
                 {pin} · configured key matches"
            ),
            (None, true) => {
                "signing key configured · matches persisted public identity".to_string()
            }
            (None, false) => {
                "no committed channel anchor and no signing configuration".to_string()
            }
        },
    );

    // ---- claim (spec §2 — before the expensive build) ----------------------
    let now = unix_now();
    let ledger_text = fs::read_to_string(repo.join(ledger::LEDGER_FILE))
        .map_err(|e| Error::new(format!("read {}: {e}", ledger::LEDGER_FILE)))?;
    let tail = ledger::tail(&ledger_text)?;
    let provisional = ledger::next_build(tail.build, now)?;
    let provisional_floor = effective_min_build(opts.min_build, newest_min_build, provisional)?;
    step(
        "floor",
        &format!(
            "operator {} · newest {} · effective {}",
            display_floor(opts.min_build),
            newest_channel.map_or_else(
                || "none".to_string(),
                |published| format!("{}: {}", published.tag, display_floor(published.min_build))
            ),
            display_floor(provisional_floor)
        ),
    );

    let (build, commit) = match kind {
        CutKind::Real => {
            step(
                "claim",
                &format!(
                    "ledger tail {} ({}) → claiming {provisional}",
                    tail.build, tail.version
                ),
            );
            let plan = ledger::ClaimPlan {
                version: &version,
                now,
                allow_existing_section: recut,
                max_attempts: ledger::MAX_CLAIM_ATTEMPTS,
            };
            let date = changelog::today_la()?;
            let repo_buf = repo.to_path_buf();
            let ver = version.clone();
            let mut regenerate = move |_n: u64| -> Result<Vec<String>> {
                if recut {
                    // Bump + roll already sit on origin (the wedged cut's
                    // commit); the recut commit is the ledger line alone.
                    return Ok(vec![]);
                }
                regen_release_files(&repo_buf, &ver, &date)
            };
            let claim = ledger::claim(&git, repo, &plan, &mut regenerate)?;
            step(
                "",
                &format!(
                    "pushed \"release: v{version} (build {})\"  [verified: origin/main == HEAD, \
                     ledger tail == \"{}\"]",
                    claim.build, claim.ledger_line
                ),
            );
            (claim.build, claim.commit)
        }
        CutKind::DryRun | CutKind::Rehearse => {
            // Provisional n: read-only — max(remote tail + 1, now), never
            // pushed (gates proved HEAD == origin/main, so the local ledger
            // IS origin's blob).
            let n = provisional;
            step(
                "claim",
                &format!(
                    "ledger tail {} ({}) → provisional {n} (no ledger push — {})",
                    tail.build,
                    tail.version,
                    if kind == CutKind::DryRun {
                        "dry-run"
                    } else {
                        "rehearsal"
                    }
                ),
            );
            (n, rev_parse(&git, "HEAD")?)
        }
    };
    // A concurrent ledger claimant can only raise `build`, but validate the
    // actual verified claim too: the persisted journal and emitted manifest
    // must be bound to the number that was really won, never the provisional.
    let min_build = effective_min_build(opts.min_build, newest_min_build, build)?;

    let mut ctx = CutCtx {
        repo: repo.to_path_buf(),
        dist,
        journal_path: journal_path.clone(),
        slug: publish_slug,
        tag: format!("v{version}"),
        notes_section: if kind == CutKind::Real {
            version.clone()
        } else {
            "Unreleased".into()
        },
        version,
        build,
        commit,
        min_build,
        arm64_only: opts.arm64_only,
        manifest_signed: false,
        signature_required: signature_policy.required,
        signature_pubkey: signature_policy.pubkey,
        release_id: None,
        draft_create_issued: false,
        upload_intents: Vec::new(),
        mirror_slug: mirror_slug.clone(),
        mirror_release_id: None,
        mirror_create_issued: false,
        mirror_upload_intents: Vec::new(),
        kind,
        lease: None,
        fence: None,
        journal: None,
    };
    if kind == CutKind::Real {
        let j = Journal {
            format: JOURNAL_FORMAT,
            version: ctx.version.clone(),
            build_number: ctx.build,
            commit: ctx.commit.clone(),
            min_build: ctx.min_build,
            arm64_only: ctx.arm64_only,
            manifest_signed: ctx.manifest_signed,
            signature_required: ctx.signature_required,
            signature_pubkey: ctx.signature_pubkey.clone(),
            release_id: None,
            draft_create_issued: false,
            upload_intents: Vec::new(),
            mirror_release_id: None,
            mirror_create_issued: false,
            mirror_upload_intents: Vec::new(),
            done: vec![],
        };
        j.save(&journal_path)?;
        ctx.journal = Some(j);
    }

    run_pipeline(&mut ctx, t0)
}

/// `--resume`: rebuild the context from the journal and re-enter at the first
/// incomplete step (spec §5).
fn resume_cut(
    repo: &Path,
    dist: &Path,
    journal_path: &Path,
    origin_slug: &str,
    journal: Journal,
    t0: Instant,
    recovered_session: Option<(ReleaseLeaseGuard, PublisherFenceGuard)>,
) -> Result<()> {
    journal.ensure_resumable()?;
    let Some(next) = journal.first_incomplete() else {
        return Err(Error::new(
            "the journaled cut already completed every step — nothing to resume \
             (delete dist/cut-state.toml)"
                .to_string(),
        ));
    };
    let git = GitCli::new(repo);
    println!(
        "aterm-release · cut v{} (build {}) — RESUME at step \"{next}\"",
        journal.version, journal.build_number
    );

    // A journal is a crash cursor, never publication authority.  Bind every
    // ordinary resume to its exact claim-commit ledger tail and origin/main,
    // and reject every unexplained worktree change before acquiring a remote
    // lease/fence.
    ordinary_resume_claim_preflight(repo, &git, &journal)?;

    // Steps that (re)bake artifact bytes additionally require the recovered
    // signing key. The claim-commit/clean-tree proof above applies to every
    // resume, including late upload/flip/verify entries.
    if !journal.is_done("build") && journal.signature_required {
        let material = load_signing_material(repo)?.ok_or_else(|| {
            Error::new(
                "signature-required resume cannot rebuild without the recovered offline signing configuration",
            )
        })?;
        if Some(material.pubkey.as_str()) != journal.signature_pubkey.as_deref() {
            return Err(Error::new(
                "resume signing key differs from the journaled actual channel public key",
            ));
        }
    }

    let (lease, fence) =
        recovered_session.map_or((None, None), |(lease, fence)| (Some(lease), Some(fence)));
    let mut ctx = CutCtx {
        repo: repo.to_path_buf(),
        dist: dist.to_path_buf(),
        journal_path: journal_path.to_path_buf(),
        slug: origin_slug.to_string(),
        version: journal.version.clone(),
        tag: format!("v{}", journal.version),
        notes_section: journal.version.clone(),
        build: journal.build_number,
        commit: journal.commit.clone(),
        min_build: journal.min_build,
        arm64_only: journal.arm64_only,
        manifest_signed: journal.manifest_signed,
        signature_required: journal.signature_required,
        signature_pubkey: journal.signature_pubkey.clone(),
        release_id: journal.release_id,
        draft_create_issued: journal.draft_create_issued,
        upload_intents: journal.upload_intents.clone(),
        mirror_slug: workspace_mirror_slug(repo)?,
        mirror_release_id: journal.mirror_release_id,
        mirror_create_issued: journal.mirror_create_issued,
        mirror_upload_intents: journal.mirror_upload_intents.clone(),
        kind: CutKind::Real,
        lease,
        fence,
        journal: Some(journal),
    };
    run_pipeline(&mut ctx, t0)
}

/// Execute the journaled steps in order, skipping completed ones. THE one
/// pipeline all cut flavors share.
fn run_pipeline(ctx: &mut CutCtx, t0: Instant) -> Result<()> {
    // Resume re-proves/reacquires exact ownership even when `lock` was already
    // journaled. The one exception is an unlock-only resume: absence may mean
    // delete landed and the journal mark crashed, so reacquiring would undo
    // convergence.
    if ctx.kind == CutKind::Real
        && ctx.journal.as_ref().and_then(Journal::first_incomplete) != Some("unlock")
    {
        if ctx.lease.is_none() {
            let git = GitCli::new(&ctx.repo);
            ctx.lease = Some(acquire_release_lease(&git, &ctx.commit)?);
        }
        if ctx.fence.is_none() {
            let git = GitCli::new(&ctx.repo);
            ctx.fence = Some(acquire_publisher_fence(&git, &ctx.commit)?);
        }
        // The pre-claim read is only an early refusal.  Channel signing state
        // can advance while the ledger CAS is racing, so the acquired session
        // must re-derive the policy before any build/upload is trusted.
        revalidate_ctx_signature_policy(ctx)?;
    }
    let result = run_pipeline_inner(ctx, t0);
    let fence_release = if let Some(fence) = ctx.fence.take() {
        release_publisher_fence(&GitCli::new(&ctx.repo), &fence).map(|_| ())
    } else {
        Ok(())
    };
    match (result, fence_release) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(fence_error)) => Err(Error::new(format!(
            "release pipeline completed, but exact publisher-fence cleanup failed: {fence_error}"
        ))),
        (Err(error), Err(fence_error)) => Err(Error::new(format!(
            "{error}; additionally, exact publisher-fence cleanup failed: {fence_error}"
        ))),
    }
}

fn run_pipeline_inner(ctx: &mut CutCtx, t0: Instant) -> Result<()> {
    for name in STEPS {
        if ctx.is_done(name) {
            continue;
        }
        if ctx.kind == CutKind::Real && !matches!(name, "lock" | "unlock") {
            ensure_ctx_release_lease(ctx)?;
        }
        match name {
            "lock" => step_lock(ctx)?,
            "build" => step_build(ctx)?,
            "selfcheck" => {
                step_selfcheck(ctx)?;
                if ctx.kind == CutKind::DryRun {
                    step(
                        "DONE",
                        &format!(
                            "dry-run: v{} (build {}) built + self-checked in dist/ — \
                             nothing committed, nothing uploaded.  [{}]",
                            ctx.version,
                            ctx.build,
                            fmt_elapsed(t0)
                        ),
                    );
                    return Ok(());
                }
            }
            "draft" => step_draft(ctx)?,
            "upload" => step_upload(ctx)?,
            "preflip" => step_preflip(ctx)?,
            "tag" => {
                // The rehearsal never tags origin; GitHub mints the scratch
                // repo's tag at flip time.
                if ctx.kind == CutKind::Real {
                    step_tag(ctx)?;
                }
            }
            "flip" => step_flip(ctx)?,
            "archive" => step_archive(ctx)?,
            "verify" => step_verify(ctx)?,
            "mirror" => step_mirror(ctx)?,
            "unlock" => {
                if ctx.kind == CutKind::Real {
                    step_unlock(ctx)?;
                }
            }
            _ => unreachable!("unknown pipeline step {name}"),
        }
        ctx.mark(name)?;
    }

    match ctx.kind {
        CutKind::Real => step(
            "DONE",
            &format!(
                "v{} (build {}) — fleet stages within 6h.  [{}]  state: dist/cut-state.toml",
                ctx.version,
                ctx.build,
                fmt_elapsed(t0)
            ),
        ),
        CutKind::Rehearse => {
            step(
                "DONE",
                &format!(
                    "rehearsal v{} (build {}) published to {}.  [{}]",
                    ctx.version,
                    ctx.build,
                    ctx.slug,
                    fmt_elapsed(t0)
                ),
            );
            let (owner, repo_name) = ctx.slug.split_once('/').unwrap_or(("OWNER", "REPO"));
            step(
                "",
                &format!(
                    "point the running v0.25 at it:  ATERM_UPDATE_OWNER={owner} \
                     ATERM_UPDATE_REPO={repo_name} aterm ctl update check"
                ),
            );
        }
        CutKind::DryRun => unreachable!("dry-run returned after selfcheck"),
    }
    Ok(())
}

/// Establish or re-prove the exact journal commit's ownership. Calling this
/// on every remote transition deliberately favors fail-closed recovery over a
/// process-local assumption: a killed process leaves the remote ref intact,
/// and only the same journal owner may resume it.
fn ensure_ctx_release_lease(ctx: &CutCtx) -> Result<()> {
    if ctx.kind != CutKind::Real {
        return Ok(());
    }
    let git = GitCli::new(&ctx.repo);
    let lease = ctx
        .lease
        .as_ref()
        .ok_or_else(|| Error::new("real release step has no acquired persistent claim lease"))?;
    let fence = ctx
        .fence
        .as_ref()
        .ok_or_else(|| Error::new("real release step has no unique publisher fence"))?;
    assert_publisher_session(&git, lease, fence)
}

/// Re-derive the signing verdict — the per-machine configuration folded with
/// the committed channel pin — while the exact owner+process token is held.
/// Equality includes the actual canonical key, not just a boolean: a cut whose
/// signing key vanished, whose signing configuration appeared, or whose
/// worktree pin changed mid-cut aborts instead of proceeding under the stale
/// key state it claimed under. This is what holds the pinned-channel invariant
/// at lock, preflip, and flip, not only at the pre-claim scan.
fn revalidate_ctx_signature_policy(ctx: &CutCtx) -> Result<()> {
    if ctx.kind != CutKind::Real {
        return Ok(());
    }
    ensure_ctx_release_lease(ctx)?;
    let observed = preflight_signature_policy(&ctx.repo)?;
    if observed.required != ctx.signature_required
        || observed.pubkey.as_deref() != ctx.signature_pubkey.as_deref()
    {
        return Err(Error::new(
            "local signing configuration or the committed channel pin changed after this \
             cut's pre-claim scan; refusing to build/upload/flip under stale signing state",
        ));
    }
    ensure_ctx_release_lease(ctx)
}

/// Journal step "lock": the create-only remote claim is already tied to the
/// journal commit, then the live channel is rescanned while ownership is held.
fn step_lock(ctx: &mut CutCtx) -> Result<()> {
    if ctx.kind != CutKind::Real {
        return Ok(());
    }
    ensure_ctx_release_lease(ctx)?;
    let newest = best_published(ctx)?;
    step(
        "lock",
        &format!(
            "{} owned by claim {} · live build {} checked under lease",
            RELEASE_LEASE_REF,
            ctx.commit,
            newest.map_or_else(|| "none".to_string(), |build| build.to_string())
        ),
    );
    Ok(())
}

/// Journal step "unlock": compare-and-swap delete against the exact claim
/// commit. `AlreadyAbsent` is the valid replay after delete landed but the
/// journal mark did not.
fn step_unlock(ctx: &mut CutCtx) -> Result<()> {
    let git = GitCli::new(&ctx.repo);
    let outcome = if let Some(fence) = ctx.fence.as_ref() {
        release_completed_publisher_session(&git, &ctx.commit, fence)?
    } else {
        release_completed_session_without_guard(&git, &ctx.commit).map_err(|error| {
            Error::new(format!(
                "{error}; after proving the old publisher stopped, use \
                 `cargo ship recover v{} {} --old-publisher-stopped` for a surviving same-claim token",
                ctx.version, ctx.commit
            ))
        })?
    };
    ctx.lease = None;
    ctx.fence = None;
    step(
        "unlock",
        match outcome {
            LeaseRelease::Released => "exact-owner remote lease released",
            LeaseRelease::AlreadyAbsent => {
                "remote lease already absent (prior CAS delete converged)"
            }
            LeaseRelease::AlreadySuperseded => {
                "prior CAS delete converged; successor lease left untouched"
            }
        },
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// pipeline steps
// ---------------------------------------------------------------------------

/// Fresh-cut release-commit content for the claim: roll the changelog in the
/// same commit as the ledger line. Cargo.toml's `[workspace.package]` version
/// and Cargo.lock stay byte-for-byte untouched — the workspace version is the
/// operator's bump, and the cut only READS it (DEV → 0) to derive the release.
///
/// Runs on origin's blobs — after a lost CAS race the claim resets hard and
/// calls this again, so it always re-reads the worktree fresh.
pub(crate) fn regen_release_files(repo: &Path, version: &str, date: &str) -> Result<Vec<String>> {
    let cl_path = repo.join(changelog::CHANGELOG_FILE);
    let cl_text = fs::read_to_string(&cl_path)
        .map_err(|e| Error::new(format!("read {}: {e}", changelog::CHANGELOG_FILE)))?;
    let rolled = changelog::roll(&cl_text, version, date)?;
    fs::write(&cl_path, rolled)
        .map_err(|e| Error::new(format!("write {}: {e}", changelog::CHANGELOG_FILE)))?;

    Ok(vec![changelog::CHANGELOG_FILE.into()])
}

/// Opt-in deep gate: `tools/verify.sh --full`, streamed (spec decisions 15/22).
fn run_gate_script(repo: &Path) -> Result<()> {
    step("gate", "tools/verify.sh --full (opt-in deep gate)");
    let status = Command::new(repo.join("tools/verify.sh"))
        .arg("--full")
        .current_dir(repo)
        .stdin(std::process::Stdio::null())
        .status()
        .map_err(|e| Error::new(format!("spawn tools/verify.sh: {e}")))?;
    if !status.success() {
        return Err(Error::new(
            "tools/verify.sh --full FAILED — fix the tree; nothing was claimed or committed"
                .to_string(),
        ));
    }
    Ok(())
}

/// Step "build": per-arch builds → lipo → dSYM → bundle → sign → DMG →
/// notarize hook → provenance → manifest + notes. One re-enterable unit whose
/// outputs are all functions of (version, build_number, claim commit).
fn step_build(ctx: &mut CutCtx) -> Result<()> {
    let conf = sign::load_default()?;
    let mut build_env = conf
        .as_ref()
        .map(sign::ReleaseConf::env_pins)
        .unwrap_or_default();
    if ctx.signature_required {
        build_env.retain(|(key, _)| key != "ATERM_UPDATE_PUBKEY");
        build_env.push((
            "ATERM_UPDATE_PUBKEY".to_string(),
            ctx.signature_pubkey
                .clone()
                .ok_or_else(|| Error::new("signed build has no persisted public key"))?,
        ));
    }

    step(
        "build",
        &format!(
            "SOURCE_DATE_EPOCH={} → aterm (ONE binary: window + session + every verb)",
            ctx.build
        ),
    );
    let plan = buildplan::BuildPlan {
        repo_root: ctx.repo.clone(),
        out_dir: ctx.dist.clone(),
        build_number: ctx.build,
        short_version: ctx.version.clone(),
        arm64_only: ctx.arm64_only,
        extra_env: build_env,
        expected_update_pin_sha256: ctx
            .signature_pubkey
            .as_deref()
            .map(update_key_fingerprint)
            .transpose()?,
    };
    let bout = buildplan::run(&plan)?;

    // The bytes must come from the claim commit, unmoved and clean — a HEAD
    // that drifted mid-build would stamp one commit and ship another.
    let git = GitCli::new(&ctx.repo);
    let head = rev_parse(&git, "HEAD")?;
    if head != ctx.commit {
        return Err(Error::new(format!(
            "HEAD moved during the build ({head} != release commit {}) — rebuild from \
             the release commit",
            ctx.commit
        )));
    }
    let stamp = bundle::git_commit_stamp(&ctx.repo);
    if stamp.ends_with("-dirty") {
        return Err(Error::new(format!(
            "the tree went dirty during the build (ATermGitCommit would stamp {stamp:?}) — \
             a release bundle must be reproducible from its commit"
        )));
    }
    step(
        "",
        &format!(
            "archs [{}] · {} · dSYM {}",
            bout.archs,
            bout.compiler_line,
            match (&bout.dsym, &bout.dsym_zip) {
                (Some(_), Some(z)) => format!("ok → {}", z.display()),
                _ => "SKIPPED (no symbolication)".to_string(),
            }
        ),
    );

    // Batteries-included seed (§9.1): validate BEFORE assemble so a cut never
    // seals a seed its own client would refuse. The gate is fail-closed both
    // ways — a seed present without the root key the client bakes is dead
    // weight plus a false "batteries included" label, so it errors rather
    // than silently shipping either half.
    let seed = match crate::seedpack::resolve(&ctx.dist) {
        Some(dir) => {
            let root_key = conf
                .as_ref()
                .and_then(|c| c.get("ATERM_PKG_ROOTKEY"))
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    std::env::var("ATERM_PKG_ROOTKEY")
                        .ok()
                        .filter(|k| !k.is_empty())
                });
            let Some(root_key) = root_key else {
                return Err(Error::new(format!(
                    "{} is present but no ATERM_PKG_ROOTKEY is configured (release.conf or env) — \
                     the client would bake no root key and ignore the seed; configure the pin or \
                     remove the seed dir",
                    dir.display()
                )));
            };
            Some(crate::seedpack::validate(&dir, &root_key).map_err(Error::new)?)
        }
        None => None,
    };

    let spec = bundle::BundleSpec {
        repo_root: ctx.repo.clone(),
        out_dir: ctx.dist.clone(),
        short_version: ctx.version.clone(),
        build_number: ctx.build,
        bundle_id: "com.aterm.aterm".to_string(),
        git_commit: stamp.clone(),
        aterm_bin: bout.aterm,
        seed,
    };
    let app = bundle::assemble(&spec)?;
    step(
        "bundle",
        &format!(
            "aterm.app: Short={}  CFBundleVersion={}  ATermGitCommit={stamp}  seed={}",
            ctx.version,
            ctx.build,
            match &spec.seed {
                Some(s) => format!(
                    "{} program(s), index_build {}",
                    s.programs.len(),
                    s.index_build
                ),
                None => "none".to_string(),
            }
        ),
    );

    let sign_id = conf.as_ref().and_then(|c| c.sign_id()).map(str::to_string);
    let signed_by = sign::sign_app(
        &app,
        &ctx.repo.join("apps/aterm-mac/aterm.entitlements"),
        sign_id.as_deref(),
    )?;
    step(
        "sign",
        &(if sign_id.is_some() {
            format!("Developer ID: {signed_by}")
        } else {
            "ad-hoc (no ~/.aterm/release.conf signing identity) — Dev-ID/notarize hook idle"
                .to_string()
        }),
    );

    let dout = dmg::create(&app, &ctx.dist, &ctx.version)?;
    sign::sign_and_notarize_dmg(&dout.path, conf.as_ref())?;
    // Re-hash AFTER the Dev-ID hook: codesign REWRITES the DMG bytes (and the
    // notarization staple appends its ticket), so the digest dmg::create
    // minted covers the pre-hook bytes only. The manifest sha256 must be
    // the digest of the exact bytes clients download — a stale one would
    // hard-abort the self-check after the whole build+notarize, and (were the
    // self-check ever skipped) fail the sha256 gate on every v0.25 client.
    // The hook only ever mutates under a configured signing identity, so the
    // default ad-hoc path keeps dmg::create's digest without a second pass.
    let (dmg_sha, dmg_size) = if sign_id.is_some() {
        let sha = dmg::sha256_file(&dout.path)?;
        let size = fs::metadata(&dout.path)
            .map_err(|e| Error::new(format!("stat {}: {e}", dout.path.display())))?
            .len();
        (sha, size)
    } else {
        (dout.sha256.clone(), dout.size_bytes)
    };
    // The updater container, from the SAME signed .app. It is built here rather
    // than from the DMG because the Dev-ID hook above rewrites only the DMG's
    // bytes — the bundle is final the moment `sign_app` returns — and because
    // `ditto` must archive the bundle directly to preserve its seal.
    let zout = dmg::create_zip(&app, &ctx.dist, &ctx.version)?;
    // Provenance AFTER signing: binary_sha256 must cover the SIGNED bytes.
    let provenance_path = bundle::write_provenance(&spec, &app, &signed_by)?;
    if ctx.signature_required {
        // Optional signing: bind the provenance to the actual signing key's
        // fingerprint. There is no permanent-authority/epoch record any more —
        // the pin is simply the configured update key.
        let fingerprint = ctx
            .signature_pubkey
            .as_deref()
            .map(update_key_fingerprint)
            .transpose()?
            .ok_or_else(|| Error::new("signed build has no persisted public key"))?;
        let mut provenance = fs::read_to_string(&provenance_path).map_err(|error| {
            Error::new(format!(
                "read {} for update-pin provenance: {error}",
                provenance_path.display()
            ))
        })?;
        provenance.push_str(&format!("update_pubkey_fingerprint_sha256={fingerprint}\n"));
        fs::write(&provenance_path, provenance).map_err(|error| {
            Error::new(format!(
                "write {} update-pin provenance: {error}",
                provenance_path.display()
            ))
        })?;
    }
    step(
        "dmg",
        &format!(
            "{} ({:.1} MB)  sha256 {}…",
            dout.path.display(),
            dmg_size as f64 / 1_000_000.0,
            &dmg_sha[..12.min(dmg_sha.len())]
        ),
    );
    step(
        "zip",
        &format!(
            "{} ({:.1} MB)  sha256 {}… — the container the in-app updater stages from",
            zout.path.display(),
            zout.size_bytes as f64 / 1_000_000.0,
            &zout.sha256[..12.min(zout.sha256.len())]
        ),
    );

    // ---- manifest + notes (the rolled body, verbatim, once — spec §3) -----
    let cl_text = fs::read_to_string(ctx.repo.join(changelog::CHANGELOG_FILE))
        .map_err(|e| Error::new(format!("read {}: {e}", changelog::CHANGELOG_FILE)))?;
    let body = changelog::rolled_body(&cl_text, &ctx.notes_section)?;
    fs::write(ctx.notes_path(), format!("{body}\n"))
        .map_err(|e| Error::new(format!("write {}: {e}", ctx.notes_path().display())))?;

    let plist_text = fs::read_to_string(app.join("Contents/Info.plist"))
        .map_err(|e| Error::new(format!("read stamped Info.plist: {e}")))?;
    let min_os = manifest_out::plist_string(&plist_text, "LSMinimumSystemVersion")
        .unwrap_or_else(|| "11.0".to_string());
    let manifest = manifest_out::build(&manifest_out::ManifestInputs {
        version: &ctx.version,
        build_number: ctx.build,
        commit: &ctx.commit,
        dmg_name: &mirror::dmg_asset_name(&ctx.version),
        dmg_sha256: &dmg_sha,
        zip_name: &mirror::zip_asset_name(&ctx.version),
        // No re-hash pass: the Dev-ID hook never touches the zip, so these are
        // the bytes clients download.
        zip_sha256: &zout.sha256,
        // The manifest's `url` must name the repository a reader can actually
        // fetch from. These same bytes ride BOTH the private release and the
        // mirrored public one, and only the public channel is readable without
        // a credential — so the channel slug wins whenever one is configured,
        // and we fall back to the publish slug only when there is no mirror
        // (a legal configuration; see mirror::update_channel_slug).
        repo_slug: &mirror::update_channel_slug(
            &fs::read_to_string(ctx.repo.join("Cargo.toml"))
                .map_err(|e| Error::new(format!("read Cargo.toml for manifest url: {e}")))?,
        )?
        .unwrap_or_else(|| ctx.slug.clone()),
        min_os: &min_os,
        team_id: &manifest_team_id(conf.as_ref()),
        pub_date: &bundle::epoch_to_rfc3339(unix_now()),
        min_build: ctx.min_build,
        changelog: &body,
    });
    let mpath = manifest_out::write(&ctx.dist, &manifest)?;
    // A re-entered build may reuse dist/. Never let an earlier signed cut's
    // detached bytes masquerade as this cut's signature when signing is now
    // disabled or fails before producing a replacement.
    let sig_path = mpath.with_extension("toml.sig");
    match fs::remove_file(&sig_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(Error::new(format!(
                "remove stale manifest signature {}: {error}",
                sig_path.display()
            )));
        }
    }
    if ctx.signature_required {
        let produced = sign_manifest_with_policy(ctx, &mpath)?;
        if produced != sig_path || !sig_path.is_file() {
            return Err(Error::new(
                "signature-required build did not produce the exact manifest signature asset",
            ));
        }
        ctx.manifest_signed = true;
    } else {
        ctx.manifest_signed = false;
    }
    if let Some(journal) = &mut ctx.journal {
        // `run_pipeline` marks `build` immediately after this returns; that
        // same atomic save persists the signature fact before later steps.
        journal.manifest_signed = ctx.manifest_signed;
        journal.signature_required = ctx.signature_required;
        journal.signature_pubkey.clone_from(&ctx.signature_pubkey);
    }
    Ok(())
}

/// team_id for the manifest: `""` = ad-hoc tier (the shipped default). When a
/// Dev-ID identity is configured, prefer the explicit conf keys, else parse
/// the "(TEAMID)" suffix of the identity, in that order.
fn manifest_team_id(conf: Option<&sign::ReleaseConf>) -> String {
    let Some(c) = conf else { return String::new() };
    let Some(id) = c.sign_id() else {
        return String::new();
    };
    for key in ["ATERM_TEAM_ID", "ATERM_EXPECTED_TEAM_ID"] {
        if let Some(t) = c.get(key)
            && !t.is_empty()
        {
            return t.to_string();
        }
    }
    // "(ABCDE12345)" — exactly 10 uppercase alphanumerics.
    id.split('(')
        .filter_map(|part| part.split(')').next())
        .find(|t| {
            t.len() == 10
                && t.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        })
        .map(str::to_string)
        .unwrap_or_default()
}

/// Step "selfcheck" (spec §7 step 4): triple build-number agreement
/// (binary == plist == manifest == n), DMG digest, codesign, the shared +
/// vendored-v0.25 manifest proof, and the client-rule monotonic check.
fn step_selfcheck(ctx: &mut CutCtx) -> Result<()> {
    let app = ctx.app_path();

    // Sealed CFBundleVersion == n.
    let plist_text = fs::read_to_string(app.join("Contents/Info.plist"))
        .map_err(|e| Error::new(format!("read stamped Info.plist: {e}")))?;
    let cf = manifest_out::plist_string(&plist_text, "CFBundleVersion")
        .ok_or_else(|| Error::new("stamped Info.plist has no CFBundleVersion".to_string()))?;
    if cf != ctx.build.to_string() {
        return Err(Error::new(format!(
            "self-check failed: CFBundleVersion {cf} != claimed build {}",
            ctx.build
        )));
    }
    let cf_short = manifest_out::plist_string(&plist_text, "CFBundleShortVersionString")
        .ok_or_else(|| {
            Error::new("stamped Info.plist has no CFBundleShortVersionString".to_string())
        })?;
    if cf_short != ctx.version {
        return Err(Error::new(format!(
            "self-check failed: CFBundleShortVersionString {cf_short:?} != claimed app version {:?}",
            ctx.version
        )));
    }

    // Binary stamp == n. The GUI binary prints no raw build number on any
    // exiting flag, but `--diagnose` prints ATERM_BUILD_TIME — which build.rs
    // derives from SOURCE_DATE_EPOCH, i.e. from n, bijectively — so equality
    // with epoch_to_rfc3339(n) proves the binary was compiled with this exact
    // claim baked in.
    let diag = Command::new(app.join("Contents/MacOS/aterm"))
        .arg("--diagnose")
        .current_dir(&ctx.repo)
        .output()
        .map_err(|e| Error::new(format!("spawn aterm --diagnose: {e}")))?;
    if !diag.status.success() {
        return Err(Error::new(format!(
            "self-check failed: the shipped binary's --diagnose probe exited {}",
            diag.status
        )));
    }
    let diag_text = String::from_utf8_lossy(&diag.stdout).into_owned();
    buildplan::validate_app_version_reports(&ctx.version, &[("shipped universal", &diag_text)])?;
    let expect_built = bundle::epoch_to_rfc3339(ctx.build);
    let built = diag_text.lines().find_map(|l| {
        l.split("built ")
            .nth(1)
            .map(|t| t.trim_end_matches(')').to_string())
    });
    if built.as_deref() != Some(expect_built.as_str()) {
        return Err(Error::new(format!(
            "self-check failed: binary build stamp {built:?} != expected {expect_built:?} \
             (from claimed n {}) — the binary was not compiled with this claim",
            ctx.build
        )));
    }

    // Every shipped argv0 identity is the same Mach-O and must agree on the
    // ledger-derived app version. Exact stdout matching rejects stale cached
    // library slices as well as alias-routing drift.
    for (basename, identity) in [
        ("aterm", "aterm"),
        ("aterm-cli", "aterm"),
        ("aterm-gui", "aterm-gui"),
        ("aterm-ctl", "aterm-ctl"),
    ] {
        let output = Command::new(app.join("Contents/MacOS").join(basename))
            .arg("--version")
            .current_dir(&ctx.repo)
            .output()
            .map_err(|error| Error::new(format!("spawn {identity} --version: {error}")))?;
        if !output.status.success() {
            return Err(Error::new(format!(
                "self-check failed: {identity} --version exited {}",
                output.status
            )));
        }
        buildplan::validate_named_cli_app_version(identity, &ctx.version, &output.stdout)?;
    }

    let provenance = fs::read(ctx.provenance_path())
        .map_err(|error| Error::new(format!("read release provenance: {error}")))?;
    validate_claim_provenance(&provenance, &ctx.version, ctx.build, &ctx.commit)?;

    if ctx.signature_required {
        // Optional signing: prove the shipped binary embedded the fingerprint of
        // the configured signing key, and that the provenance records it. There
        // is no permanent authority or epoch metadata to bind against.
        let fingerprint = ctx
            .signature_pubkey
            .as_deref()
            .map(update_key_fingerprint)
            .transpose()?
            .ok_or_else(|| Error::new("signed channel has no persisted public key"))?;
        buildplan::validate_slice_update_pin_reports(
            &fingerprint,
            &[("shipped universal", &diag_text)],
        )?;
        let provenance = fs::read_to_string(ctx.provenance_path())
            .map_err(|error| Error::new(format!("read update-pin provenance: {error}")))?;
        let expected = format!("update_pubkey_fingerprint_sha256={fingerprint}");
        if !provenance.lines().any(|line| line == expected) {
            return Err(Error::new(format!(
                "release provenance is missing exact update-pin field {expected:?}"
            )));
        }
        step(
            "",
            &format!(
                "binary runtime reports pinned update key {}…; per-slice/provenance proof bound",
                &fingerprint[..12]
            ),
        );
    }

    // Manifest (the bytes ON DISK — what will be uploaded) == n, digest, and
    // the shared + vendored-v0.25 parse proof.
    let mtext = fs::read_to_string(ctx.manifest_path())
        .map_err(|e| Error::new(format!("read {}: {e}", ctx.manifest_path().display())))?;
    let manifest = Manifest::parse(&mtext)
        .map_err(|e| Error::new(format!("self-check: manifest re-parse failed: {e}")))?;
    if manifest.build_number != ctx.build
        || manifest.version != ctx.version
        || manifest.commit.as_deref() != Some(ctx.commit.as_str())
    {
        return Err(Error::new(format!(
            "self-check failed: manifest identity ({}, {}, {:?}) != claimed ({}, {}, {})",
            manifest.version,
            manifest.build_number,
            manifest.commit,
            ctx.version,
            ctx.build,
            ctx.commit
        )));
    }

    let sig_path = ctx.manifest_path().with_extension("toml.sig");
    if ctx.signature_required {
        if !ctx.manifest_signed {
            return Err(Error::new(
                "self-check failed: signed-channel journal does not record a signature",
            ));
        }
        let signature = fs::read(&sig_path)
            .map_err(|error| Error::new(format!("read {}: {error}", sig_path.display())))?;
        verify_detached_manifest_signature(
            ctx.signature_pubkey.as_deref().ok_or_else(|| {
                Error::new("self-check: signed channel has no persisted public key")
            })?,
            mtext.as_bytes(),
            &signature,
        )?;
    } else if ctx.manifest_signed || sig_path.exists() {
        return Err(Error::new(
            "self-check failed: unsigned channel carries an unexpected signature artifact",
        ));
    }

    // DMG bytes == manifest sha256 (re-hashed from disk, in-process).
    let sha = dmg::sha256_file(&ctx.dmg_path())?;
    if !sha.eq_ignore_ascii_case(&manifest.sha256) {
        return Err(Error::new(format!(
            "self-check failed: DMG sha256 {sha} != manifest {}",
            manifest.sha256
        )));
    }

    // Same proof for the updater container: it is the artifact the whole fleet
    // downloads, so a stale/absent zip must abort the cut here, not strand every
    // machine on a digest mismatch after publication.
    let zip_name = mirror::zip_asset_name(&ctx.version);
    match (manifest.zip.as_deref(), manifest.zip_sha256.as_deref()) {
        (Some(name), Some(expected)) => {
            if name != zip_name {
                return Err(Error::new(format!(
                    "self-check failed: manifest names zip {name:?}, expected {zip_name:?}"
                )));
            }
            let sha = dmg::sha256_file(&ctx.zip_path())?;
            if !sha.eq_ignore_ascii_case(expected) {
                return Err(Error::new(format!(
                    "self-check failed: zip sha256 {sha} != manifest {expected}"
                )));
            }
        }
        _ => {
            return Err(Error::new(
                "self-check failed: manifest carries no zip name + digest pair; the in-app \
                 updater cannot stage without `hdiutil`, which an orphaned post-handoff \
                 process cannot use",
            ));
        }
    }

    // codesign — the hard gate (sign.rs's inline verify print is advisory).
    let cs = Command::new("codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(&app)
        .output()
        .map_err(|e| Error::new(format!("spawn codesign --verify: {e}")))?;
    if !cs.status.success() {
        return Err(Error::new(format!(
            "self-check failed: codesign --verify --deep --strict: {}",
            String::from_utf8_lossy(&cs.stderr).trim()
        )));
    }
    // Team-ID + spctl, iff signing (spec §7 step 4).
    let team = manifest.team_id.clone().unwrap_or_default();
    if !team.is_empty() {
        let dv = Command::new("codesign")
            .args(["-dv", "--verbose=2"])
            .arg(&app)
            .output()
            .map_err(|e| Error::new(format!("spawn codesign -dv: {e}")))?;
        let info = format!(
            "{}{}",
            String::from_utf8_lossy(&dv.stderr),
            String::from_utf8_lossy(&dv.stdout)
        );
        if !info.contains(&format!("TeamIdentifier={team}")) {
            return Err(Error::new(format!(
                "self-check failed: bundle TeamIdentifier does not match manifest team_id {team}"
            )));
        }
        let sp = Command::new("spctl")
            .args(["-a", "-t", "exec"])
            .arg(&app)
            .output()
            .map_err(|e| Error::new(format!("spawn spctl: {e}")))?;
        if !sp.status.success() {
            return Err(Error::new(format!(
                "self-check failed: spctl assessment rejected the signed app: {}",
                String::from_utf8_lossy(&sp.stderr).trim()
            )));
        }
    }
    step(
        "selfcheck",
        &format!(
            "binary == plist == manifest == {} · codesign --verify --deep --strict ok",
            ctx.build
        ),
    );

    // Monotonic build + carried floor vs the newest-first client scan.
    let best = best_published(ctx)?;
    step(
        "",
        &format!(
            "manifest bytes parse (shared type + vendored v0.25 fixture) · > published {}",
            best.map_or("none".to_string(), |b| b.to_string())
        ),
    );
    Ok(())
}

/// Replay the client selection against the publish target and apply both the
/// monotonic-build gate and the carried-floor gate; returns the selected live
/// build for the transcript.
fn best_published(ctx: &CutCtx) -> Result<Option<u64>> {
    let scanned = if ctx.kind == CutKind::Rehearse {
        verify::scan_published(&ctx.slug, true)?
    } else {
        verify::scan_published_in_repo(&ctx.repo, &ctx.slug, true)?
    };
    let best = scanned.first();
    let newest_floor = best.and_then(|published| published.min_build);
    if ctx.kind == CutKind::Real {
        let guard = ctx.lease.as_ref().ok_or_else(|| {
            Error::new("PublishChecked requires an acquired release lease".to_string())
        })?;
        let fence = ctx.fence.as_ref().ok_or_else(|| {
            Error::new("PublishChecked requires a unique publisher fence".to_string())
        })?;
        let git = GitCli::new(&ctx.repo);
        assert_publisher_session(&git, guard, fence)?;
        let observed_owner = release_lease_owner(&git)?;
        publish_checked(
            guard,
            observed_owner.as_deref(),
            ctx.min_build,
            newest_floor,
        )?;
    } else {
        channel_floor_covered(ctx.min_build, newest_floor)?;
    }
    monotonic_ok(ctx.build, &ctx.tag, best.map(|p| (p.tag.as_str(), p.build)))?;
    Ok(best.map(|p| p.build))
}

/// Step "draft" (spec §7 step 5, first half): create the draft release
/// targeting the claim sha. Draft-first is what closes the half-upload
/// window: no client rule ever selects a draft.
fn step_draft(ctx: &mut CutCtx) -> Result<()> {
    if ctx.kind == CutKind::Rehearse {
        // The scratch repo needs the release commit before --target can bind
        // to it. Force-push: the scratch repo's history is disposable.
        step(
            "publish",
            &format!("pushing HEAD to scratch repo {} (rehearsal)", ctx.slug),
        );
        let git = GitCli::new(&ctx.repo);
        let url = format!("https://github.com/{}.git", ctx.slug);
        git_ok(&git, &["push", "--force", &url, "HEAD:refs/heads/main"]).map_err(|e| {
            Error::new(format!(
                "cannot push to the rehearsal repo (create it first: \
                 gh repo create {} --private): {e}",
                ctx.slug
            ))
        })?;
    }
    let observed = unique_release_object_by_tag(&ctx.slug, &ctx.tag)?;
    let release = match durable_post_decision(ctx.draft_create_issued, observed.is_some()) {
        DurablePostDecision::PersistIntentThenPost => {
            let release = create_draft(ctx)?;
            step(
                "publish",
                &format!(
                    "draft {} created (--target {})",
                    ctx.tag,
                    &ctx.commit[..12.min(ctx.commit.len())]
                ),
            );
            release
        }
        DurablePostDecision::AwaitVisibility => {
            return Err(Error::new(format!(
                "draft create intent for {} was already durably issued, but the object is not yet visible; refusing a duplicate POST (resume after GitHub converges or use explicit stopped-publisher recovery)",
                ctx.tag
            )));
        }
        DurablePostDecision::ConvergeVisible if observed.as_ref().is_some_and(|r| r.draft) => {
            let release = observed.expect("visible draft decision");
            validate_release_object_capability(
                Some(&release),
                release.id,
                &ctx.tag,
                &ctx.commit,
                true,
            )?;
            step(
                "publish",
                &format!(
                    "draft {} ID {} already exists — exact target re-proven",
                    ctx.tag, release.id
                ),
            );
            release
        }
        DurablePostDecision::ConvergeVisible => {
            return Err(Error::new(format!(
                "{} is already PUBLISHED on {} — a published release is never overwritten; \
                 retire a bad build with `cargo ship yank <build>`",
                ctx.tag, ctx.slug
            )));
        }
    };
    ctx.bind_release_id(release.id)?;
    let reread = release_object_by_id(&ctx.slug, release.id)?;
    validate_release_object_capability(reread.as_ref(), release.id, &ctx.tag, &ctx.commit, true)?;
    if ctx.kind == CutKind::Real {
        ensure_ctx_release_lease(ctx)?;
        if remote_annotated_tag(&GitCli::new(&ctx.repo), &ctx.tag)?.is_some() {
            return Err(Error::new(format!(
                "draft creation unexpectedly materialized git tag {}; refusing to journal the draft step before the late exact annotated-tag protocol",
                ctx.tag
            )));
        }
    }
    Ok(())
}

/// One direct REST draft-create attempt, never [`gh_retry`] or the high-level
/// `gh release create` command: a client-side timeout can report failure for a create that
/// LANDED server-side, and GitHub happily mints a SECOND draft with the same
/// tag_name (drafts don't own their tag until the flip) — the orphan would
/// linger forever, keep `release_state` answering Draft for a version with no
/// cut in flight, and survive `--abandon` (which deletes only the draft gh
/// resolves). Durable intent is saved before the POST; this invocation then
/// probes once, and a later resume may discover but never recreate it.
fn create_draft(ctx: &mut CutCtx) -> Result<ReleaseObjectIdentity> {
    let notes = fs::read_to_string(ctx.notes_path())
        .map_err(|error| Error::new(format!("read draft release notes: {error}")))?;
    let title = format!("aterm {}", ctx.version);
    let endpoint = format!("{GITHUB_API_ORIGIN}/repos/{}/releases", ctx.slug);
    let payload = serde_json::to_vec(&serde_json::json!({
        "tag_name": ctx.tag.as_str(),
        "target_commitish": ctx.commit.as_str(),
        "name": title,
        "body": notes,
        "draft": true,
        "prerelease": false,
    }))
    .map_err(|error| Error::new(format!("serialize draft release request: {error}")))?;
    let post = OneShotPost::prepare_json("create", "draft release request", &endpoint, &payload)?;
    // Every fallible preflight precedes the durable edge. The returned
    // non-cloneable permit is consumed by the immediately following POST.
    ensure_ctx_release_lease(ctx)?;
    let permit = ctx.persist_draft_create_intent()?;
    // Creation is deliberately attempted at most once per invocation. A
    // timeout followed by an eventually-consistent empty list cannot prove
    // the POST did not land; retrying here can mint a duplicate draft.
    let out = post.issue(permit)?;
    if out.success() {
        let release = parse_release_object_response(&out.stdout)?;
        validate_release_object_capability(
            Some(&release),
            release.id,
            &ctx.tag,
            &ctx.commit,
            true,
        )?;
        return Ok(release);
    }
    if let Some(release) = unique_release_object_by_tag(&ctx.slug, &ctx.tag)? {
        validate_release_object_capability(
            Some(&release),
            release.id,
            &ctx.tag,
            &ctx.commit,
            true,
        )?;
        return Ok(release);
    }
    Err(Error::new(format!(
        "draft create returned {} but no exact release object is visible for {}; refusing an ambiguous retry in this invocation (resume after GitHub converges): {}",
        if out.success() { "success" } else { "failure" },
        ctx.tag,
        out.stderr_utf8().trim()
    )))
}

/// Step "upload": converge every exact-name asset through a durable one-shot
/// intent. A lost POST response can delay resume, but can never duplicate or
/// overwrite an object.
fn step_upload(ctx: &mut CutCtx) -> Result<()> {
    // A completed selfcheck journal entry is only historical evidence. Local
    // dist/ is mutable and ignored by git, so re-run the full proof before a
    // resumed upload can read a single byte from it.
    step_selfcheck(ctx)?;
    let release_id = ctx.required_release_id("upload")?;
    let release = release_object_by_id(&ctx.slug, release_id)?;
    validate_release_object_capability(release.as_ref(), release_id, &ctx.tag, &ctx.commit, true)?;
    // Draft-first re-proof (spec decision 4): "draft" may be journaled done by
    // a CRASHED attempt whose release was since finished — and possibly
    // republished under a fresh build — from another machine. Only step_draft
    // carries the Published guard, and resume skips it; without this re-check
    // a stale journal could still issue a new upload request against a LIVE
    // release in front of the whole fleet.
    if verify::release_state(&ctx.slug, &ctx.tag)? == verify::ReleaseState::Published {
        return Err(Error::new(format!(
            "{} is already PUBLISHED on {} — refusing to upload over a live release; \
             this journal is stale (the cut was finished elsewhere). Delete \
             dist/cut-state.toml; retire a bad live build with `cargo ship yank <build>`",
            ctx.tag, ctx.slug
        )));
    }
    let mut files: Vec<PathBuf> = vec![
        ctx.dmg_path(),
        ctx.zip_path(),
        ctx.manifest_path(),
        ctx.provenance_path(),
    ];
    let sig = ctx.manifest_path().with_extension("toml.sig");
    match (ctx.signature_required, ctx.manifest_signed, sig.is_file()) {
        (true, true, true) => {
            let manifest = fs::read(ctx.manifest_path()).map_err(|error| {
                Error::new(format!("read {}: {error}", ctx.manifest_path().display()))
            })?;
            let signature = fs::read(&sig)
                .map_err(|error| Error::new(format!("read {}: {error}", sig.display())))?;
            verify_detached_manifest_signature(
                ctx.signature_pubkey.as_deref().ok_or_else(|| {
                    Error::new("upload: signed channel has no persisted public key")
                })?,
                &manifest,
                &signature,
            )?;
            files.push(sig);
        }
        (false, false, false) => {}
        _ => {
            return Err(Error::new(
                "manifest signature disk/journal/ratchet state disagrees; refusing opportunistic upload",
            ));
        }
    }
    if ctx.dsym_zip_path().is_file() {
        files.push(ctx.dsym_zip_path());
    }
    for f in &files {
        if !f.is_file() {
            return Err(Error::new(format!(
                "asset missing: {} — the build step's outputs are gone; delete \
                 dist/cut-state.toml and run a plain `cargo ship cut` to recut",
                f.display()
            )));
        }
    }
    for file in &files {
        upload_release_asset_by_id(ctx, release_id, file)?;
    }
    step(
        "",
        &format!(
            "{} assets converged on immutable draft release ID {release_id}",
            files.len()
        ),
    );
    Ok(())
}

fn upload_release_asset_by_id(ctx: &mut CutCtx, release_id: u64, file: &Path) -> Result<()> {
    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::new("release asset filename is not UTF-8"))?;
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(Error::new(format!(
            "release asset name {name:?} is outside the exact upload URL alphabet"
        )));
    }

    let prior_intent = ctx.upload_intent_issued(name);
    let observed = release_asset_identity_for_release_id_optional(&ctx.slug, release_id, name)?;
    match durable_post_decision(prior_intent, observed.is_some()) {
        DurablePostDecision::AwaitVisibility => {
            return Err(Error::new(format!(
                "upload intent for draft asset {name} was already durably issued, but the asset is not visible; refusing a duplicate POST (resume after GitHub converges)"
            )));
        }
        DurablePostDecision::PersistIntentThenPost => {}
        DurablePostDecision::ConvergeVisible => {
            let (old_id, _) = observed.expect("visible asset decision");
            if verify_release_asset_id_matches_local(&ctx.slug, release_id, name, file).is_ok() {
                return Ok(());
            }
            if prior_intent {
                return Err(Error::new(format!(
                    "draft asset {name} exists with wrong bytes after its durable upload intent; refusing delete/re-upload because a prior POST may be the authority"
                )));
            }
            let release = release_object_by_id(&ctx.slug, release_id)?;
            validate_release_object_capability(
                release.as_ref(),
                release_id,
                &ctx.tag,
                &ctx.commit,
                true,
            )?;
            ensure_ctx_release_lease(ctx)?;
            let endpoint = format!("repos/{}/releases/assets/{old_id}", ctx.slug);
            let out = gh_raw(&["api", "--method", "DELETE", &endpoint])?;
            match release_asset_identity_for_release_id_optional(&ctx.slug, release_id, name)? {
                None => {}
                Some((observed, _)) if observed != old_id => {
                    return Err(Error::new(format!(
                        "draft asset {name} was replaced while deleting exact asset ID {old_id}; refusing to delete the replacement"
                    )));
                }
                Some(_) => {
                    return Err(Error::new(format!(
                        "delete exact draft asset ID {old_id} failed: {}",
                        out.stderr_utf8().trim()
                    )));
                }
            }
        }
    }

    if durable_post_decision(prior_intent, false) != DurablePostDecision::PersistIntentThenPost {
        return Err(Error::new(format!(
            "upload intent for draft asset {name} cannot authorize another POST after convergence"
        )));
    }
    let endpoint = exact_release_upload_url(&ctx.slug, release_id, name)?;
    let post = OneShotPost::prepare_binary("release asset", &endpoint, file)?;
    let release = release_object_by_id(&ctx.slug, release_id)?;
    validate_release_object_capability(release.as_ref(), release_id, &ctx.tag, &ctx.commit, true)?;
    ensure_ctx_release_lease(ctx)?;
    let permit = ctx.persist_upload_intent(name)?;
    // Like draft creation, an upload POST is issued once per invocation. An
    // absent immediate probe after timeout may be visibility lag, not proof
    // of non-delivery; resume will first converge on any exact-name object.
    let out = post.issue(permit)?;
    if release_asset_identity_for_release_id_optional(&ctx.slug, release_id, name)?.is_some() {
        verify_release_asset_id_matches_local(&ctx.slug, release_id, name, file)?;
        return Ok(());
    }
    Err(Error::new(format!(
        "exact-ID upload of {name} returned {} but no asset is visible; refusing an ambiguous duplicate retry in this invocation (resume after GitHub converges): {}",
        if out.success() { "success" } else { "failure" },
        out.stderr_utf8().trim()
    )))
}

pub fn exact_release_upload_url(slug: &str, release_id: u64, name: &str) -> Result<String> {
    let valid_slug = slug.split_once('/').is_some_and(|(owner, repo)| {
        !owner.is_empty()
            && !repo.is_empty()
            && !owner.contains('/')
            && !repo.contains('/')
            && owner
                .bytes()
                .chain(repo.bytes())
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    });
    if !valid_slug
        || release_id == 0
        || name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(Error::new(
            "release upload owner/repo, ID, or asset name is not canonical",
        ));
    }
    // `gh api --hostname uploads.github.com` incorrectly treats that host as
    // a GitHub Enterprise name and prefixes `api.`. An absolute endpoint is
    // explicitly accepted by gh and preserves GitHub's dedicated upload host.
    Ok(format!(
        "{GITHUB_UPLOAD_ORIGIN}/repos/{slug}/releases/{release_id}/assets?name={name}"
    ))
}

/// Re-prove the complete invisible publication object from immutable IDs.
/// This is called both at `preflip` and again inside `flip`, so a crash or a
/// replacement after either earlier journal mark cannot make mutable local or
/// remote bytes visible without a fresh proof.
fn prove_draft_artifacts(ctx: &mut CutCtx) -> Result<()> {
    step_selfcheck(ctx)?;
    let release_id = ctx.required_release_id("draft artifact proof")?;
    let before = release_object_by_id(&ctx.slug, release_id)?;
    validate_release_object_capability(before.as_ref(), release_id, &ctx.tag, &ctx.commit, true)?;
    let manifest_text = fs::read_to_string(ctx.manifest_path()).map_err(|error| {
        Error::new(format!(
            "read local manifest for draft proof {}: {error}",
            ctx.manifest_path().display()
        ))
    })?;
    let manifest = Manifest::parse(&manifest_text)
        .map_err(|error| Error::new(format!("parse local manifest for draft proof: {error}")))?;
    let provenance_path = ctx.provenance_path();
    let provenance_name = provenance_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::new("provenance filename is not UTF-8"))?
        .to_string();
    let dsym_path = ctx.dsym_zip_path();
    let dsym_name = dsym_path
        .is_file()
        .then(|| dsym_path.file_name().and_then(|name| name.to_str()))
        .flatten();
    let inventory_before = release_asset_inventory_for_release_id(&ctx.slug, release_id)?;
    let names: Vec<String> = inventory_before
        .iter()
        .map(|asset| asset.name.clone())
        .collect();
    validate_draft_asset_set(
        &names,
        &manifest,
        ctx.signature_required,
        &provenance_name,
        dsym_name,
    )?;

    let mut files = vec![
        ctx.manifest_path(),
        ctx.dmg_path(),
        ctx.zip_path(),
        ctx.provenance_path(),
    ];
    if ctx.signature_required {
        files.push(ctx.manifest_path().with_extension("toml.sig"));
    }
    if ctx.dsym_zip_path().is_file() {
        files.push(ctx.dsym_zip_path());
    }
    for file in &files {
        let name = file
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| Error::new("draft artifact filename is not UTF-8"))?;
        verify_release_asset_id_matches_local(&ctx.slug, release_id, name, file)?;
    }
    let inventory_after = release_asset_inventory_for_release_id(&ctx.slug, release_id)?;
    if inventory_after != inventory_before {
        return Err(Error::new(
            "draft asset name/immutable-ID/size inventory changed during byte verification",
        ));
    }
    let after = release_object_by_id(&ctx.slug, release_id)?;
    validate_release_object_capability(after.as_ref(), release_id, &ctx.tag, &ctx.commit, true)?;
    Ok(())
}

/// Step "preflip" (spec §7 step 5): re-run the monotonic client-rule check
/// right before anything becomes visible.
fn step_preflip(ctx: &mut CutCtx) -> Result<()> {
    prove_draft_artifacts(ctx)?;
    revalidate_ctx_signature_policy(ctx)?;
    let best = best_published(ctx)?;
    step(
        "",
        &format!(
            "pre-flip: client-rule selection (\"newest non-draft carrying {}\") still tops \
             out {} — below {} ok",
            manifest_out::MANIFEST_ASSET,
            best.map_or("at none".to_string(), |b| format!("at {b}")),
            ctx.build
        ),
    );
    Ok(())
}

/// Step "tag" (real cut only): the LATE annotated tag (spec decision 5) —
/// pushed only now, with all assets up and the pre-flip check green, so a
/// failed cut never leaves a public tag.
fn step_tag(ctx: &mut CutCtx) -> Result<()> {
    let git = GitCli::new(&ctx.repo);
    let tag_ref = format!("refs/tags/{}", ctx.tag);
    let existing = git.git(&[
        "rev-parse",
        "-q",
        "--verify",
        &format!("{}^{{commit}}", ctx.tag),
    ])?;
    if existing.success() {
        // Resume: a local tag from the crashed attempt is fine iff it points
        // at OUR commit; anything else would re-point a name we're publishing.
        let at = existing.stdout_utf8().trim().to_string();
        if at != ctx.commit {
            return Err(Error::new(format!(
                "local tag {} points at {at}, not the release commit {} — delete it \
                 (git tag -d {}) and resume",
                ctx.tag, ctx.commit, ctx.tag
            )));
        }
    } else {
        git_ok(
            &git,
            &[
                "tag",
                "-a",
                &ctx.tag,
                "-m",
                &format!("aterm {} (build {})", ctx.version, ctx.build),
                &ctx.commit,
            ],
        )?;
    }
    let local_token = rev_parse(&git, &format!("refs/tags/{}", ctx.tag))?;
    let local_type = git_ok(&git, &["cat-file", "-t", &local_token])?;
    if local_type.stdout_utf8().trim() != "tag" {
        return Err(Error::new(format!(
            "local {} is not an annotated tag object; refusing to publish a lightweight tag",
            ctx.tag
        )));
    }
    let local_peel = rev_parse(&git, &format!("{local_token}^{{commit}}"))?;
    if local_peel != ctx.commit {
        return Err(Error::new(format!(
            "captured annotated tag object {local_token} peels to {local_peel}, not claim {}",
            ctx.commit
        )));
    }
    ensure_ctx_release_lease(ctx)?;
    git_ok(
        &git,
        &["push", "origin", &format!("{local_token}:{tag_ref}")],
    )?;
    let remote = remote_annotated_tag(&git, &ctx.tag)?.ok_or_else(|| {
        Error::new(format!(
            "remote {} is absent or not annotated after push",
            ctx.tag
        ))
    })?;
    if remote.commit != ctx.commit {
        return Err(Error::new(format!(
            "remote annotated tag {} peels to {}, not claim {}",
            ctx.tag, remote.commit, ctx.commit
        )));
    }
    step("", &format!("tag {} pushed", ctx.tag));
    Ok(())
}

/// Step "flip": draft → live. The single instant the release becomes visible
/// to the fleet — everything before it was invisible, everything after it is
/// verification. Because a resume can re-enter here long after the crashed
/// attempt journaled "preflip", the state AND the monotonic rule are both
/// re-proven now, not trusted from the journal.
fn step_flip(ctx: &mut CutCtx) -> Result<()> {
    let release_id = ctx.required_release_id("flip")?;
    match release_object_by_id(&ctx.slug, release_id)? {
        Some(release) if release.draft => {
            validate_release_object_capability(
                Some(&release),
                release_id,
                &ctx.tag,
                &ctx.commit,
                true,
            )?;
            prove_draft_artifacts(ctx)?;
            // Spec §7 step 5 mandates the client-rule monotonic check
            // IMMEDIATELY before visibility: a newer build may have shipped
            // in the days since this journal's "preflip" ran — abort here,
            // while aborting is still invisible, instead of flipping a
            // never-selectable release that would need a yank.
            revalidate_ctx_signature_policy(ctx)?;
            best_published(ctx)?;
            let git = GitCli::new(&ctx.repo);
            let remote_tag = remote_annotated_tag(&git, &ctx.tag)?.ok_or_else(|| {
                Error::new(format!(
                    "remote annotated tag {} vanished immediately before flip",
                    ctx.tag
                ))
            })?;
            if remote_tag.commit != ctx.commit {
                return Err(Error::new(format!(
                    "remote annotated tag {} peels to {}, not claim {}; refusing visibility",
                    ctx.tag, remote_tag.commit, ctx.commit
                )));
            }
            // Keep the owner check adjacent to the visibility mutation too;
            // a deleted or foreign-replaced lease is never papered over by a
            // process-local guard obtained at step entry.
            let endpoint = format!("repos/{}/releases/{release_id}", ctx.slug);
            gh_retry_guarded(
                &["api", "--method", "PATCH", &endpoint, "-F", "draft=false"],
                || {
                    prove_draft_artifacts(ctx)?;
                    let current = release_object_by_id(&ctx.slug, release_id)?;
                    validate_release_object_capability(
                        current.as_ref(),
                        release_id,
                        &ctx.tag,
                        &ctx.commit,
                        true,
                    )?;
                    let tag = remote_annotated_tag(&GitCli::new(&ctx.repo), &ctx.tag)?.ok_or_else(
                        || Error::new("remote annotated tag vanished before flip retry"),
                    )?;
                    if tag.commit != ctx.commit {
                        return Err(Error::new(
                            "remote annotated tag changed claim before flip retry",
                        ));
                    }
                    ensure_ctx_release_lease(ctx)?;
                    Ok(())
                },
            )?;
            let after = release_object_by_id(&ctx.slug, release_id)?;
            validate_release_object_capability(
                after.as_ref(),
                release_id,
                &ctx.tag,
                &ctx.commit,
                false,
            )?;
            step("", &format!("draft release ID {release_id} → live"));
        }
        Some(release) => {
            validate_release_object_capability(
                Some(&release),
                release_id,
                &ctx.tag,
                &ctx.commit,
                false,
            )?;
            // Already live: EITHER our own flip landed and the crash ate the
            // journal mark (converge silently), OR a stale journal is
            // replaying against a release someone else published under this
            // tag — only the live build number distinguishes the two, and
            // claiming another cut's release as ours would break the
            // draft-first invariant end to end.
            let live = published_build(ctx)?;
            if live != Some(ctx.build) {
                return Err(Error::new(format!(
                    "{} is already PUBLISHED on {} carrying build {}, not our {} — \
                     this journal is stale (the cut was finished/republished \
                     elsewhere); delete dist/cut-state.toml",
                    ctx.tag,
                    ctx.slug,
                    live.map_or("<unreadable>".to_string(), |b| b.to_string()),
                    ctx.build
                )));
            }
            step(
                "",
                "already live (the flip landed before the crash) — converged",
            );
        }
        None => {
            return Err(Error::new(format!(
                "exact release ID {release_id} ({}) vanished from {} before the flip — it was deleted or \
                 abandoned elsewhere; delete dist/cut-state.toml and run a plain \
                 `cargo ship cut` to recut with a fresh number",
                ctx.tag, ctx.slug
            )));
        }
    }
    Ok(())
}

/// Step "archive": converge the release channel to one exact-name appcast.
/// This runs only after our release is published, so the current tag remains
/// continuously discoverable while every older published manifest/signature
/// is metadata-renamed to its deterministic per-tag archive name. The step is
/// journaled as a unit; each individual PATCH is itself convergent, and the
/// final fresh listing proves the invariant before verify may proceed.
fn step_archive(ctx: &mut CutCtx) -> Result<()> {
    let release_id = ctx.required_release_id("archive")?;
    let release = release_object_by_id(&ctx.slug, release_id)?;
    validate_release_object_capability(release.as_ref(), release_id, &ctx.tag, &ctx.commit, false)?;
    let live_manifest =
        download_release_asset_for_release_id(&ctx.slug, release_id, manifest_out::MANIFEST_ASSET)?;
    let live_signature = if ctx.signature_required {
        Some(download_release_asset_for_release_id(
            &ctx.slug,
            release_id,
            manifest_out::MANIFEST_SIG_ASSET,
        )?)
    } else {
        if release_asset_identity_for_release_id_optional(
            &ctx.slug,
            release_id,
            manifest_out::MANIFEST_SIG_ASSET,
        )?
        .is_some()
        {
            return Err(Error::new(
                "unsigned archive target carries an unexpected exact signature asset",
            ));
        }
        None
    };
    let local_manifest = fs::read(ctx.manifest_path()).map_err(|error| {
        Error::new(format!(
            "read journaled manifest {} before archive: {error}",
            ctx.manifest_path().display()
        ))
    })?;
    let signature_path = ctx.manifest_path().with_extension("toml.sig");
    let local_signature = if ctx.signature_required {
        Some(fs::read(&signature_path).map_err(|error| {
            Error::new(format!(
                "read journaled signature {} before archive: {error}",
                signature_path.display()
            ))
        })?)
    } else {
        None
    };
    let live = validate_live_release_identity(
        ExpectedReleaseIdentity {
            version: &ctx.version,
            build: ctx.build,
            commit: &ctx.commit,
        },
        &live_manifest,
        live_signature.as_deref(),
        Some(&local_manifest),
        local_signature.as_deref(),
        ctx.signature_required,
        ctx.signature_pubkey.as_deref(),
    )
    .map_err(|error| {
        Error::new(format!(
            "refusing archive for {}: {error}; no historical asset was changed",
            ctx.tag
        ))
    })?;
    verify_release_asset_id_matches_local(&ctx.slug, release_id, &live.dmg, &ctx.dmg_path())?;
    if let Some(zip) = live.zip.as_deref() {
        verify_release_asset_id_matches_local(&ctx.slug, release_id, zip, &ctx.zip_path())?;
    }
    verify_release_asset_id_matches_local(
        &ctx.slug,
        release_id,
        ctx.provenance_path()
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| Error::new("provenance filename is not UTF-8"))?,
        &ctx.provenance_path(),
    )?;
    // Keep the exact process-token check immediately adjacent to the first
    // historical PATCH; validation can involve several authenticated reads.
    ensure_ctx_release_lease(ctx)?;
    let lease = ctx
        .lease
        .as_ref()
        .ok_or_else(|| Error::new("archive has no persistent claim lease"))?;
    let fence = ctx
        .fence
        .as_ref()
        .ok_or_else(|| Error::new("archive has no unique publisher fence"))?;
    let mut remote = GhAppcastArchiveRemote::fenced(&ctx.slug, &ctx.repo, lease, fence);
    let renamed =
        converge_appcast_archive_with_policy(&mut remote, &ctx.tag, ctx.signature_required)?;
    step(
        "archive",
        &format!(
            "{renamed} historical appcast asset{} metadata-renamed · {} is sole exact head",
            if renamed == 1 { "" } else { "s" },
            ctx.tag
        ),
    );
    Ok(())
}

/// The build number the release under OUR tag carries live, read from its
/// manifest asset (`None` only when the name is absent or its bounded bytes are
/// syntactically unreadable). Duplicate names, oversize metadata, identity
/// races, and transport failures remain hard errors — the one fact that tells
/// "our own half-flipped cut" apart from "someone else's release wearing our
/// tag" must never be guessed through ambiguity.
fn published_build(ctx: &CutCtx) -> Result<Option<u64>> {
    let release_id = ctx.required_release_id("published-build convergence proof")?;
    if release_asset_identity_for_release_id_optional(
        &ctx.slug,
        release_id,
        manifest_out::MANIFEST_ASSET,
    )?
    .is_none()
    {
        return Ok(None);
    }
    let bytes =
        download_release_asset_for_release_id(&ctx.slug, release_id, manifest_out::MANIFEST_ASSET)?;
    let Ok(text) = String::from_utf8(bytes) else {
        return Ok(None);
    };
    Ok(Manifest::parse(&text).ok().map(|m| m.build_number))
}

// ---------------------------------------------------------------------------
// step "mirror": copy the verified release to the PUBLIC update channel
// ---------------------------------------------------------------------------

/// Pre-claim proof that the public update channel is reachable AND writable by
/// this operator's credential.
///
/// Deliberately runs before the ledger claim. The mirror itself is the last
/// remote step of a cut, so discovering "no push permission on the channel"
/// there would burn a build number, leave a live private release the fleet
/// cannot see, and hold the lease until an OWNER-level permission grant — which
/// is not something a resume can fix. Failing here costs nothing.
pub fn preflight_mirror_target(slug: &str) -> Result<()> {
    let endpoint = format!("repos/{slug}");
    let out = gh_retry(&[
        "api",
        &endpoint,
        "--jq",
        r#"[(.private | tostring), (.permissions.push // false | tostring)] | @tsv"#,
    ])
    .map_err(|error| {
        Error::new(format!(
            "cannot read the public update channel {slug} named by {table} {key}: {error}. \
             A 404 here means the repository does not exist or this account cannot see it; \
             create it (public) and grant the release account write access.",
            table = mirror::CHANNEL_TABLE,
            key = mirror::CHANNEL_KEY,
        ))
    })?;
    let row = out.stdout_utf8();
    let row = row.trim();
    let mut fields = row.split('\t');
    let (Some(private), Some(push), None) = (fields.next(), fields.next(), fields.next()) else {
        return Err(Error::new(format!(
            "public update channel {slug} returned a malformed repository row {row:?}"
        )));
    };
    if push != "true" {
        return Err(Error::new(format!(
            "the authenticated account has no push permission on the public update \
             channel {slug}, so `cargo ship cut` cannot mirror this release there and \
             the fleet would never see it. This is an OWNER action, not a resume: grant \
             the release account write access to {slug} (or clear \
             `{table} {key}` in Cargo.toml to publish without a public mirror — shipped \
             builds would then read the private repo and need a token). Refusing before \
             the ledger claim so no build number is burned.",
            table = mirror::CHANNEL_TABLE,
            key = mirror::CHANNEL_KEY,
        )));
    }
    if private == "true" {
        return Err(Error::new(format!(
            "the update channel {slug} is PRIVATE. Mirroring there would reproduce the \
             very failure this channel exists to remove: a shipped build with no \
             provisioned credential cannot read a private repo's releases and silently \
             never updates. Make {slug} public, or point \
             `{table} {key}` at a public repository.",
            table = mirror::CHANNEL_TABLE,
            key = mirror::CHANNEL_KEY,
        )));
    }
    Ok(())
}

/// Validate a MIRROR release object capability. The private side's
/// [`validate_release_object_capability`] also binds `target_commitish` to the
/// claim sha; the mirror deliberately cannot, because the public channel is a
/// different repository whose history does not contain the claim commit at all
/// (its releases are anchored at the public default branch). Identity there is
/// the immutable release ID plus the tag plus the draft state — the three
/// things a mutation must not have raced.
fn validate_mirror_release_capability(
    observed: Option<&ReleaseObjectIdentity>,
    expected_id: u64,
    expected_tag: &str,
    expected_draft: bool,
) -> Result<()> {
    let observed = observed.ok_or_else(|| {
        Error::new(format!(
            "mirror release ID {expected_id} vanished before mutation"
        ))
    })?;
    if observed.id != expected_id
        || observed.tag != expected_tag
        || observed.draft != expected_draft
    {
        return Err(Error::new(format!(
            "mirror release ID {expected_id} changed tag/state; refusing mutation"
        )));
    }
    Ok(())
}

/// Step "mirror" — the step that makes auto-update actually work.
///
/// It runs AFTER `verify`, so the private release is already live and fully
/// proven, and BEFORE `unlock`, so the cut still holds its lease/fence and a
/// failure is resumable rather than abandoned. The copy is draft-first and
/// digest-verified exactly like the private publish: create one draft under a
/// durable one-shot intent, upload each client-required asset once by immutable
/// ID, re-download every one of them and prove it byte-identical to the local
/// artifact, prove the exact asset set the updater elects, and only then flip.
///
/// Failure is a cut failure on purpose. The compiled-in channel of every shipped
/// binary is the mirror, so a release that reaches the private repo and not the
/// channel is invisible to the fleet — indistinguishable, from a user's Mac,
/// from no release at all. That is the silent-never-updates bug; it must be
/// loud and it must be resumable.
fn step_mirror(ctx: &mut CutCtx) -> Result<()> {
    // Rehearsals publish to a scratch repo and must never touch the real public
    // channel; dry-runs have already returned after selfcheck.
    if ctx.kind != CutKind::Real {
        return Ok(());
    }
    let Some(slug) = ctx.mirror_slug.clone() else {
        step(
            "mirror",
            &format!(
                "no {} {} declared — clients read {} directly; nothing to mirror",
                mirror::CHANNEL_TABLE,
                mirror::CHANNEL_KEY,
                ctx.slug
            ),
        );
        return Ok(());
    };
    if slug == ctx.slug {
        step(
            "mirror",
            &format!("update channel is the publish repo {slug} — already published there"),
        );
        return Ok(());
    }
    ensure_ctx_release_lease(ctx)?;
    // EVERYTHING below this line talks to the public channel and nothing else: the
    // asset bytes come from local `dist/` files, and the two `ctx.slug` uses above are
    // a message and the equality guard. So the release-org credential is safe to hold
    // for the whole step, and it drops on every exit path including `?`.
    let _cred = ChannelCred::enter();
    preflight_mirror_target(&slug)?;

    let observed = unique_release_object_by_tag(&slug, &ctx.tag)?;
    let release_id = match mirror::mirror_plan(
        ctx.mirror_create_issued,
        observed.as_ref().map(|release| release.draft),
    ) {
        mirror::MirrorPlan::AwaitVisibility => {
            return Err(Error::new(format!(
                "mirror create intent for {} on {slug} was already durably issued, but no \
                 release object is visible; refusing a duplicate POST. Re-run \
                 `cargo ship cut --resume` after GitHub converges.",
                ctx.tag
            )));
        }
        mirror::MirrorPlan::CreateDraft => {
            let release = create_mirror_draft(ctx, &slug)?;
            step(
                "mirror",
                &format!("draft {} created on public channel {slug}", ctx.tag),
            );
            release.id
        }
        mirror::MirrorPlan::ConvergeDraft => {
            let release = observed.expect("visible draft decision");
            // A draft we never issued a create POST for is not ours to adopt.
            // The journal refuses to bind an object ID without the matching
            // durable intent (that pairing is what makes the one-shot protocol
            // meaningful), so say WHY here instead of failing later inside a
            // journal save with an opaque invariant message.
            if !ctx.mirror_create_issued {
                return Err(Error::new(format!(
                    "a draft release for {} already exists on the public channel {slug} \
                     (ID {}) but this cut never issued a create POST for it — it is a \
                     leftover or foreign object, and adopting it would bind a capability \
                     with no durable intent. Inspect and delete it, then \
                     `cargo ship cut --resume`.",
                    ctx.tag, release.id
                )));
            }
            step(
                "mirror",
                &format!(
                    "draft {} ID {} already on {slug} — converging",
                    ctx.tag, release.id
                ),
            );
            release.id
        }
        mirror::MirrorPlan::ConvergePublished => {
            // Our own flip landed and the journal mark did not — the only
            // benign reading. Prove the live channel head really is THIS build
            // before treating it as ours; anything else is a foreign release
            // sitting on our tag, and adopting it would publish someone else's
            // bytes as this cut.
            let release = observed.expect("visible published decision");
            prove_mirror_channel_head(ctx, &slug, release.id)?;
            step(
                "mirror",
                &format!(
                    "{} already live on {slug} carrying build {} — converged",
                    ctx.tag, ctx.build
                ),
            );
            return Ok(());
        }
    };
    ctx.bind_mirror_release_id(release_id)?;
    let reread = release_object_by_id(&slug, release_id)?;
    validate_mirror_release_capability(reread.as_ref(), release_id, &ctx.tag, true)?;

    for file in ctx.mirror_asset_paths() {
        if !file.is_file() {
            return Err(Error::new(format!(
                "mirror asset missing: {} — this cut's dist/ artifacts are gone, so the \
                 public channel cannot be served the same bytes that were verified; \
                 recover the cut rather than mirroring different bytes",
                file.display()
            )));
        }
        upload_mirror_asset(ctx, &slug, release_id, &file)?;
    }

    // Prove, from a FRESH remote listing, that the draft carries exactly the
    // asset set the deployed updater elects — and that every one of those
    // objects is byte-identical to the artifact `verify` just proved live on
    // the private repo. Both proofs happen while the release is still a draft:
    // a channel head is never allowed to become visible unproven.
    prove_mirror_draft_assets(ctx, &slug, release_id)?;

    let endpoint = format!("repos/{slug}/releases/{release_id}");
    gh_retry_guarded(
        &["api", "--method", "PATCH", &endpoint, "-F", "draft=false"],
        || {
            let current = release_object_by_id(&slug, release_id)?;
            validate_mirror_release_capability(current.as_ref(), release_id, &ctx.tag, true)?;
            ensure_ctx_release_lease(ctx)?;
            Ok(())
        },
    )?;
    let after = release_object_by_id(&slug, release_id)?;
    validate_mirror_release_capability(after.as_ref(), release_id, &ctx.tag, false)?;
    prove_mirror_channel_head(ctx, &slug, release_id)?;
    // Everything above ran through `gh`, i.e. WITH the release-org credential. That
    // proves the release exists; it does NOT prove the thing this step's message
    // claims and the whole mirror exists for — that a machine with no credential at
    // all can read it. A private (or membership-restricted) mirror passes every
    // authenticated proof above and is invisible to every real client, which is the
    // silent never-updates failure the mirror was built to remove.
    prove_channel_is_anonymously_readable(ctx, &slug)?;
    step(
        "mirror",
        &format!(
            "v{} (build {}) is live on the public channel {slug} — every install \
             updates from here, no token required",
            ctx.version, ctx.build
        ),
    );
    Ok(())
}

/// Prove a CREDENTIAL-LESS client can actually read this channel's newest release
/// and fetch its assets — the one property the authenticated proofs cannot see.
///
/// Deliberately uses `curl` rather than `gh`: `gh` always attaches a credential,
/// so it can never answer this question. The request carries no `Authorization`
/// header and the token-bearing environment variables are cleared for the child,
/// so an ambient `GH_TOKEN`/`GITHUB_TOKEN` in the cutter's shell cannot make an
/// unreadable channel look readable.
///
/// Fails CLOSED: a network failure here is reported as a failure to prove, not as
/// proof. Better to refuse a cut than to publish a channel nobody can read.
/// How long the anonymous post-flip probes keep retrying, and how often.
///
/// A draft flipped live does NOT become anonymously readable atomically: the
/// release object, the asset listing, and the download CDN each converge within
/// seconds of each other. Observed on the v0.8.0 cut — the DMG's unauthenticated
/// URL 404'd at probe time and served correct bytes moments later, failing a cut
/// whose artifacts were already complete and byte-correct.
///
/// Retrying does not weaken the proof. The property is "a credential-less client
/// can fetch this", and a client arriving seconds after the flip is the real case,
/// not a lenient one. A genuinely incomplete upload fails every attempt and the
/// cut still refuses — it just takes [`ANON_PROBE_ATTEMPTS`] tries to say so.
const ANON_PROBE_ATTEMPTS: u32 = 10;

/// Gap between anonymous probe attempts.
const ANON_PROBE_DELAY: Duration = Duration::from_secs(6);

/// Run one anonymous `curl` probe, retrying while it fails.
///
/// Credentials are stripped from the child on every attempt: the whole point is to
/// see the channel exactly as an install with no token sees it. See
/// [`ANON_PROBE_ATTEMPTS`] for why retrying is sound.
fn anon_probe(args: &[&str]) -> Result<std::process::Output> {
    let mut last = None;
    for attempt in 1..=ANON_PROBE_ATTEMPTS {
        let out = Command::new("curl")
            .args(args)
            // Strip every credential the child could otherwise pick up. curl does not
            // read these itself, but clearing them keeps the intent explicit and
            // survives someone later swapping curl for a helper that does.
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_ENTERPRISE_TOKEN")
            .env_remove("NETRC")
            .output()
            .map_err(|error| Error::new(format!("spawn anonymous probe: {error}")))?;
        if out.status.success() {
            return Ok(out);
        }
        last = Some(out);
        if attempt < ANON_PROBE_ATTEMPTS {
            std::thread::sleep(ANON_PROBE_DELAY);
        }
    }
    Ok(last.expect("at least one attempt"))
}

fn prove_channel_is_anonymously_readable(ctx: &CutCtx, slug: &str) -> Result<()> {
    let url = format!("{GITHUB_API_ORIGIN}/repos/{slug}/releases/tags/{}", ctx.tag);
    let out = anon_probe(&[
        "--silent",
        "--show-error",
        "--fail",
        "--location",
        "--max-time",
        "60",
        "--header",
        "Accept: application/vnd.github+json",
        &url,
    ])?;
    if !out.status.success() {
        return Err(Error::new(format!(
            "the public channel {slug} is NOT readable without a credential: an \
             unauthenticated GET of {url} failed ({}). Every authenticated check \
             above passed, so the release exists — it is simply invisible to real \
             installs, which is the silent never-updates state the mirror exists to \
             prevent. Make {slug} public (or repoint \
             `{table} {key}`), then `cargo ship cut --resume`.",
            String::from_utf8_lossy(&out.stderr).trim(),
            table = mirror::CHANNEL_TABLE,
            key = mirror::CHANNEL_KEY,
        )));
    }
    // The release object is readable; prove the ASSET BYTES are too. A release can
    // be listed while its asset download 404s (an upload that never completed), and
    // the client fails on exactly that.
    // Match against a whitespace-stripped copy so the check does not depend on
    // GitHub's JSON formatting (it currently pretty-prints `"name": "x"`, but the
    // compact form is equally valid and a formatting change must not turn this
    // proof into a spurious cut failure). Keying on the `"name":"…"` PAIR rather
    // than the bare asset name also keeps release-note prose — which routinely
    // mentions the DMG filename — from masquerading as an uploaded asset.
    let body: String = String::from_utf8_lossy(&out.stdout)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    for name in mirror::required_asset_names(&ctx.version, ctx.signature_required) {
        if !body.contains(&format!("\"name\":\"{name}\"")) {
            return Err(Error::new(format!(
                "the anonymous view of {slug} {} does not list the required asset \
                 {name}; a credential-less client would not find it",
                ctx.tag
            )));
        }
    }
    let dmg = mirror::dmg_asset_name(&ctx.version);
    let dmg_url = format!(
        "https://github.com/{slug}/releases/download/{}/{dmg}",
        ctx.tag
    );
    // This is the probe that raced GitHub's download CDN on the v0.8.0 cut: the
    // release listed the asset while `releases/download/...` still 404'd, and the
    // cut failed with everything already published and byte-correct.
    let head = anon_probe(&[
        "--silent",
        "--show-error",
        "--fail",
        "--location",
        "--head",
        "--max-time",
        "60",
        &dmg_url,
    ])?;
    if !head.status.success() {
        return Err(Error::new(format!(
            "the public channel {slug} lists {dmg} but an unauthenticated fetch of \
             {dmg_url} failed ({}) after {ANON_PROBE_ATTEMPTS} attempts over ~{}s; \
             installs would elect this release and then be unable to download it",
            String::from_utf8_lossy(&head.stderr).trim(),
            ANON_PROBE_ATTEMPTS as u64 * ANON_PROBE_DELAY.as_secs(),
        )));
    }
    Ok(())
}

/// One direct REST draft-create against the mirror, under the same one-shot
/// durable-intent contract as [`create_draft`].
///
/// Unlike the private side this sends NO `target_commitish`: the claim commit
/// does not exist in the public repository, and naming it would either fail the
/// POST or (worse) bind the release to an unrelated object. GitHub anchors the
/// tag at the channel repo's default branch when the draft is flipped, which is
/// the correct meaning — the tag on the channel is a distribution marker, and
/// the authenticity of the bytes comes from the manifest digest + optional
/// pinned signature + codesign, never from the release's target.
fn create_mirror_draft(ctx: &mut CutCtx, slug: &str) -> Result<ReleaseObjectIdentity> {
    let notes = fs::read_to_string(ctx.notes_path())
        .map_err(|error| Error::new(format!("read mirror release notes: {error}")))?;
    let title = format!("aterm {}", ctx.version);
    let endpoint = format!("{GITHUB_API_ORIGIN}/repos/{slug}/releases");
    let payload = serde_json::to_vec(&serde_json::json!({
        "tag_name": ctx.tag.as_str(),
        "name": title,
        "body": notes,
        "draft": true,
        "prerelease": false,
    }))
    .map_err(|error| Error::new(format!("serialize mirror release request: {error}")))?;
    let post = OneShotPost::prepare_json("mirror", "mirror release request", &endpoint, &payload)?;
    // Every fallible preflight precedes the durable edge; the non-cloneable
    // permit is consumed by the POST that immediately follows.
    ensure_ctx_release_lease(ctx)?;
    let permit = ctx.persist_mirror_create_intent()?;
    let out = post.issue(permit)?;
    if out.success() {
        let release = parse_release_object_response(&out.stdout)?;
        validate_mirror_release_capability(Some(&release), release.id, &ctx.tag, true)?;
        return Ok(release);
    }
    if let Some(release) = unique_release_object_by_tag(slug, &ctx.tag)? {
        validate_mirror_release_capability(Some(&release), release.id, &ctx.tag, true)?;
        return Ok(release);
    }
    Err(Error::new(format!(
        "mirror draft create returned failure and no exact release object is visible for {} \
         on {slug}; refusing an ambiguous retry in this invocation (resume after GitHub \
         converges): {}",
        ctx.tag,
        out.stderr_utf8().trim()
    )))
}

/// Converge one exact-name asset onto the mirrored draft under a durable
/// one-shot intent. Structurally the same contract as
/// [`upload_release_asset_by_id`], with one deliberate difference: an existing
/// object with the WRONG bytes is never deleted and re-uploaded. On the private
/// side that recovery exists because the draft is the only copy; here the
/// authority already exists on the private repo, so a mismatch means something
/// unexpected is holding our tag on the public channel and the safe move is to
/// stop and let a human look.
fn upload_mirror_asset(ctx: &mut CutCtx, slug: &str, release_id: u64, file: &Path) -> Result<()> {
    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::new("mirror asset filename is not UTF-8"))?;
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(Error::new(format!(
            "mirror asset name {name:?} is outside the exact upload URL alphabet"
        )));
    }
    let prior_intent = ctx.mirror_upload_intent_issued(name);
    let observed = release_asset_identity_for_release_id_optional(slug, release_id, name)?;
    match durable_post_decision(prior_intent, observed.is_some()) {
        DurablePostDecision::AwaitVisibility => {
            return Err(Error::new(format!(
                "mirror upload intent for {name} was already durably issued, but the asset is \
                 not visible; refusing a duplicate POST (resume after GitHub converges)"
            )));
        }
        DurablePostDecision::ConvergeVisible => {
            verify_release_asset_id_matches_local(slug, release_id, name, file).map_err(
                |error| {
                    Error::new(format!(
                        "mirror asset {name} on {slug} already exists with different bytes than \
                         the verified release artifact; refusing to overwrite a public-channel \
                         object. Inspect release ID {release_id} on {slug} by hand: {error}"
                    ))
                },
            )?;
            return Ok(());
        }
        DurablePostDecision::PersistIntentThenPost => {}
    }

    let endpoint = exact_release_upload_url(slug, release_id, name)?;
    let post = OneShotPost::prepare_binary("mirror asset", &endpoint, file)?;
    let release = release_object_by_id(slug, release_id)?;
    validate_mirror_release_capability(release.as_ref(), release_id, &ctx.tag, true)?;
    ensure_ctx_release_lease(ctx)?;
    let permit = ctx.persist_mirror_upload_intent(name)?;
    let out = post.issue(permit)?;
    if release_asset_identity_for_release_id_optional(slug, release_id, name)?.is_some() {
        verify_release_asset_id_matches_local(slug, release_id, name, file)?;
        return Ok(());
    }
    Err(Error::new(format!(
        "mirror upload of {name} returned {} but no asset is visible on {slug}; refusing an \
         ambiguous duplicate retry in this invocation (resume after GitHub converges): {}",
        if out.success() { "success" } else { "failure" },
        out.stderr_utf8().trim()
    )))
}

/// Prove the still-invisible mirrored draft carries EXACTLY the asset set the
/// deployed updater elects, and that each of those objects is byte-identical to
/// the local artifact `verify` proved live on the private repo.
fn prove_mirror_draft_assets(ctx: &CutCtx, slug: &str, release_id: u64) -> Result<()> {
    let before = release_object_by_id(slug, release_id)?;
    validate_mirror_release_capability(before.as_ref(), release_id, &ctx.tag, true)?;
    let inventory_before = release_asset_inventory_for_release_id(slug, release_id)?;
    let names: Vec<String> = inventory_before
        .iter()
        .map(|asset| asset.name.clone())
        .collect();
    mirror::validate_mirror_asset_set(&names, &ctx.version, ctx.signature_required)?;
    for file in ctx.mirror_asset_paths() {
        let name = file
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| Error::new("mirror artifact filename is not UTF-8"))?;
        verify_release_asset_id_matches_local(slug, release_id, name, &file)?;
    }
    let inventory_after = release_asset_inventory_for_release_id(slug, release_id)?;
    if inventory_after != inventory_before {
        return Err(Error::new(
            "mirror asset name/immutable-ID/size inventory changed during byte verification",
        ));
    }
    let after = release_object_by_id(slug, release_id)?;
    validate_mirror_release_capability(after.as_ref(), release_id, &ctx.tag, true)?;
    Ok(())
}

/// Replay the DEPLOYED CLIENT's election against the live public channel and
/// require that it lands on this cut.
///
/// This is the acceptance test for the whole feature: not "we uploaded some
/// files", but "a machine running this updater, with no token, now resolves
/// exactly this build". It re-checks the elected release's tag, its exact asset
/// set, and byte-identity of the manifest the client would download.
fn prove_mirror_channel_head(ctx: &CutCtx, slug: &str, release_id: u64) -> Result<()> {
    let live = release_object_by_id(slug, release_id)?;
    validate_mirror_release_capability(live.as_ref(), release_id, &ctx.tag, false)?;
    let names: Vec<String> = release_asset_inventory_for_release_id(slug, release_id)?
        .into_iter()
        .map(|asset| asset.name)
        .collect();
    mirror::validate_mirror_asset_set(&names, &ctx.version, ctx.signature_required)?;

    // `stop_early: true` IS the client's replay: canonical tags only, exact
    // `aterm-appcast.toml` only, greatest numeric tag wins regardless of REST
    // row order — and it downloads exactly the one manifest a real updater
    // would fetch.
    //
    // The CHANNEL scan is the required one here, not `scan_published`. A mirrored
    // release's `target_commitish` is the channel's default branch, because the
    // claim commit does not exist in that repository at all (see
    // `create_mirror_draft`, which sends no target for exactly this reason, and
    // `validate_mirror_release_capability` for the channel-side invariant).
    let published = verify::scan_published_channel(slug, true)?;
    let head = verify::select_newest(&published).ok_or_else(|| {
        Error::new(format!(
            "public channel {slug} elects no release at all after mirroring v{} — installed \
             copies would still report no update",
            ctx.version
        ))
    })?;
    if head.tag != ctx.tag {
        return Err(Error::new(format!(
            "public channel {slug} elects {}, not this cut's {}; the fleet would install a \
             different build than the one just verified",
            head.tag, ctx.tag
        )));
    }
    if head.version != ctx.version || head.build != ctx.build {
        return Err(Error::new(format!(
            "the manifest the public channel {slug} serves carries v{} build {}, not this \
             cut's v{} build {}",
            head.version, head.build, ctx.version, ctx.build
        )));
    }
    if head.asset != manifest_out::MANIFEST_ASSET {
        return Err(Error::new(format!(
            "public channel {slug} head resolved through asset {:?}, not the exact \
             {} the client requires",
            head.asset,
            manifest_out::MANIFEST_ASSET
        )));
    }
    let local_manifest = fs::read_to_string(ctx.manifest_path()).map_err(|error| {
        Error::new(format!(
            "read local manifest for mirror head proof {}: {error}",
            ctx.manifest_path().display()
        ))
    })?;
    if head.text != local_manifest {
        return Err(Error::new(format!(
            "the manifest served by the public channel {slug} is not byte-identical to this \
             cut's dist/{}",
            manifest_out::MANIFEST_ASSET
        )));
    }
    Ok(())
}

/// Step "verify" (spec §7 step 7): the full post-publish check, shared with
/// the standalone `cargo ship verify`.
fn step_verify(ctx: &mut CutCtx) -> Result<()> {
    let signature = ctx.manifest_path().with_extension("toml.sig");
    verify::post_publish(
        &ctx.repo,
        &ctx.slug,
        &ctx.version,
        Some(ctx.build),
        Some(&ctx.manifest_path()),
        ctx.kind == CutKind::Rehearse,
        verify::PostPublishSignature {
            expected: Some(ctx.signature_required),
            pubkey: ctx.signature_pubkey.as_deref(),
            local_signature: Some(&signature),
        },
    )
}
