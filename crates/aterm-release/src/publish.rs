// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Publish (release spec §7 steps 5–6) and the whole `cut` pipeline. A cut publishes
//! once, straight onto the release channel installed copies read: the annotated tag
//! goes to origin, then `publish` ([`step_publish`]) uploads every asset to the
//! channel's release by immutable ID under durable intents and makes it the head with
//! one guarded PATCH, so no client sees a half-uploaded release. Every step is journaled
//! in `dist/cut-state.toml` for `--resume`/recut/abandon (spec §5).
//!
//! One orchestrator ([`run_cut`]) drives all three cut flavors — real, resume,
//! `--dry-run` — through the SAME step list; a dry run stops after the self-check.

use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aterm_digest::Sha256;
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};

use aterm_update_core::Manifest;
use aterm_update_core::roster;
use aterm_update_core::tag::TagError;

use crate::ledger::{self, Error, GitCli, GitRunner, Result, RunOut, git_ok, rev_parse};
use crate::{
    artifact, buildplan, bundle, changelog, channel, dmg, gates, machines, manifest_out, sign,
    verify,
};

// ---------------------------------------------------------------------------
// CLI-facing surface
// ---------------------------------------------------------------------------

/// Every `cut` flag (spec §5), parsed by cli.rs. `PartialEq` serves the CLI parse
/// tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CutOptions {
    /// Path to the one credentials profile (`--release-credentials`); only a path here:
    /// the loaded material lives in `CutCtx`, and only the derived public identity is
    /// ever journaled.
    pub release_credentials: Option<PathBuf>,
    /// Gates + a byte copy of the verified bundle this clone's release built of the
    /// checkout's commit, under its reserved number (claimed nowhere), signed into
    /// dist/ and notarized by Apple; nothing committed or published, nothing built.
    pub dry_run: bool,
    /// Re-enter the journaled cut at its first incomplete step.
    pub resume: bool,
    /// Requested operator apply floor / yank. The emitted floor is the maximum
    /// of this value and the newest live channel manifest's carried floor.
    pub min_build: Option<u64>,
    /// Both native Linux workers' handoff directory, fixed before claim.
    pub linux_artifacts: Option<PathBuf>,
    pub linux_targets: Vec<String>,
    /// `--linux-worker ARCH=DESTINATION:REPOSITORY` specs: native hosts the
    /// cutter drives itself after the claim ([`buildplan::linux::Worker`]).
    pub linux_workers: Vec<String>,
    /// `--mac-only`: the operator's explicit statement that this cut ships no
    /// Linux build. A real cut must say either this or
    /// `--linux-artifacts` ([`cli`]'s parse refuses the silence), because a cut
    /// that simply omitted Linux left every Linux install stranded on its old
    /// build — v0.86.0 through v0.93.0 all shipped that way unnoticed.
    pub mac_only: bool,
}

/// Which cut flavor is running — decided once, checked per step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutKind {
    /// The real thing: claim pushed, tag pushed to origin, published on the channel.
    Real,
    /// Stop after the self-check; nothing pushed or uploaded anywhere.
    DryRun,
}

/// How an operator runs a cut: the one spelling every "resume", "abandon" or "cut
/// again" remedy names. It runs in the operator's own shell; the cut reads its source
/// from git objects and writes containers with no extended attributes, so the launch
/// context cannot change what it builds.
pub const CUT_COMMAND: &str = crate::release::RELEASE_COMMAND;

/// How an operator runs the cutter's other verbs — `recover`, `yank`, `status`,
/// `verify`.
pub const SHIP_COMMAND: &str = "targo --unverified ship";

// ---------------------------------------------------------------------------
// transcript printing
// ---------------------------------------------------------------------------

/// The column every value starts in: two spaces of indent, the label, then one hard
/// space (a `{:<11}` pad alone vanishes for an 11-character label).
pub const VALUE_COL: usize = 13;

/// The longest label the grid carries: 2 indent + 10 + 1 hard space = [`VALUE_COL`].
/// [`grid_block_at`] debug-asserts it.
pub const LABEL_MAX: usize = VALUE_COL - 3;

/// How wide a transcript line may be: `COLUMNS` when the shell exports it, else 100,
/// clamped to 60..=120. Not a tty probe, so a piped run (`provision 2>&1 | tee`) wraps
/// the way the terminal does.
fn width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(100)
        .clamp(60, 120)
}

/// Break one message into value-column rows:
///
/// 1. Segment on `'\n'` first; an author's newline is always honoured.
/// 2. A segment's leading spaces, plus any list marker, become its hanging indent.
/// 3. Break on `' '` only. A token longer than the budget goes out whole and
///    overruns, so paths, base64 keys, URLs and commands stay pasteable.
/// 4. An empty segment is an empty line: no gutter, no trailing spaces.
/// 5. A trailing space survives, because a prompt ends `"… [y/N] "`.
fn wrapped(msg: &str, width: usize) -> Vec<String> {
    let budget = width.saturating_sub(VALUE_COL);
    let mut out: Vec<String> = Vec::new();
    for seg in msg.split('\n') {
        if seg.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let lead = seg.len() - seg.trim_start_matches(' ').len();
        let body = &seg[lead..];
        let marker = &body[..bullet_marker(body)];
        let hang = " ".repeat(lead + marker.chars().count());
        let mut line = String::from(&seg[..lead]);
        line.push_str(marker);
        let mut filled = false;
        let mut rest = &body[marker.len()..];
        while !rest.is_empty() {
            // The run of spaces before this word is kept, not collapsed: a double space
            // is the transcript's mini-column separator (`key  <path>  0600`).
            let gap = rest.len() - rest.trim_start_matches(' ').len();
            rest = &rest[gap..];
            if rest.is_empty() {
                break;
            }
            let end = rest.find(' ').unwrap_or(rest.len());
            let (word, tail) = rest.split_at(end);
            rest = tail;
            if filled && line.chars().count() + gap + word.chars().count() > budget {
                out.push(std::mem::replace(&mut line, hang.clone()));
                filled = false;
            }
            if filled {
                for _ in 0..gap {
                    line.push(' ');
                }
            }
            line.push_str(word);
            filled = true;
        }
        out.push(line);
    }
    // Rule 5: the greedy split above drops the trailing space a prompt depends on.
    if msg.ends_with(' ')
        && !msg.trim().is_empty()
        && let Some(last) = out.last_mut()
        && !last.is_empty()
    {
        last.push(' ');
    }
    out
}

/// The byte length of a leading list marker (`· `, `• `, `- `, `* `, `1. `), or 0.
/// Bytes, because it splits the segment; the marker's column width is
/// `chars().count()`, and the two differ for `· ` (3 bytes, 2 columns).
fn bullet_marker(body: &str) -> usize {
    for m in ["\u{b7} ", "\u{2022} ", "- ", "* "] {
        if body.starts_with(m) {
            return m.len();
        }
    }
    let digits = body.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && body[digits..].starts_with(". ") {
        return digits + 2;
    }
    0
}

/// Exactly what [`step`] prints, with no trailing newline, so a prompt written to
/// `/dev/tty` sits in the transcript's own grid.
pub fn grid_block(label: &str, msg: &str) -> String {
    grid_block_at(width(), label, msg)
}

/// [`grid_block`] at an explicit width, so a test can assert what an 80-column window
/// shows without mutating `COLUMNS` out from under every other test in the process.
pub fn grid_block_at(width: usize, label: &str, msg: &str) -> String {
    debug_assert!(
        label.chars().count() <= LABEL_MAX,
        "label {label:?} is {} columns; the grid carries {LABEL_MAX} \
         (a longer one eats its own separator — see VALUE_COL)",
        label.chars().count()
    );
    // Nothing to say prints nothing — a labelled empty row reads as lost output.
    if msg.trim().is_empty() {
        return String::new();
    }
    let gutter = " ".repeat(VALUE_COL);
    let mut out = String::new();
    let mut label_placed = false;
    for (i, row) in wrapped(msg, width).iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if row.is_empty() {
            continue;
        }
        if label_placed {
            out.push_str(&gutter);
        } else {
            out.push_str(&format!("  {label:<LABEL_MAX$} "));
            label_placed = true;
        }
        out.push_str(row);
    }
    out
}

/// One transcript line (or block): two-space indent, label, value at [`VALUE_COL`],
/// continuations aligned under the value. Continuation lines pass `""`. Call sites
/// never pad or hand-break a line; the width is decided here, once.
pub fn step(label: &str, msg: &str) {
    println!("{}", grid_block(label, msg));
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

/// Read the source tree's `[workspace.package]` `MAJOR.MINOR.0` version. A cut never
/// rewrites it; [`release_version_from_workspace`] derives the release version from it.
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

/// The version a cut publishes: `[workspace.package] version` as written, which must be
/// `MAJOR.MINOR.0`, the only shape `pub publish` publishes and what every build reports
/// (`aterm_types::version::app_version()`). A non-zero patch is refused, not rewritten,
/// because the binary would report it. The ledger supplies only the build number.
pub fn release_version_from_workspace(workspace: &str) -> Result<String> {
    let (_, _, patch) = version_components(workspace).map_err(|error| {
        Error::new(format!(
            "Cargo.toml [workspace.package] version is not canonical MAJOR.MINOR.0: {error}"
        ))
    })?;
    if patch != 0 {
        return Err(Error::new(format!(
            "Cargo.toml [workspace.package] version is {workspace}, and a release is \
             MAJOR.MINOR.0 — the version every build reports, which `pub publish` only \
             publishes with a 0 patch. Bump the MINOR (`pub bump aterm --minor --write`)."
        )));
    }
    Ok(workspace.to_string())
}

/// The next release version after `release`: bump MINOR, reset the third
/// component to 0. `"0.2.0"` → `"0.3.0"`. Used only to tell the operator what
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

/// "owner/repo" from `[workspace.package] repository`: the origin the ledger, lease,
/// fence and tags live on, and [`channel_slug`]'s fallback.
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

/// `[workspace.package] repository` of the checkout at `repo`, as `OWNER/REPO` — the
/// origin the ledger, the lease, the fence and the tags live on.
fn workspace_repo_slug(repo: &Path) -> Result<String> {
    let path = repo.join("Cargo.toml");
    let cargo_text = fs::read_to_string(&path)
        .map_err(|error| Error::new(format!("read {}: {error}", path.display())))?;
    repo_slug(&cargo_text).ok_or_else(|| {
        Error::new(format!(
            "{} [workspace.package] repository is not an exact GitHub OWNER/REPO URL",
            path.display()
        ))
    })
}

/// The release channel (`OWNER/REPO`) of `cargo_toml`:
/// `[workspace.metadata.aterm] update_channel` (aterm-release's channel tests hold it
/// equal to the client's compiled-in default), else `[workspace.package] repository`.
/// Resume and recovery re-read it from the tree, not the journal: it is tracked policy
/// at the claim commit, not per-cut state.
pub fn channel_slug(cargo_toml: &str) -> Result<String> {
    if let Some(slug) = channel::update_channel_slug(cargo_toml)? {
        return Ok(slug);
    }
    repo_slug(cargo_toml).ok_or_else(|| {
        Error::new(
            "Cargo.toml declares no update_channel and its [workspace.package] repository is \
             not an exact GitHub OWNER/REPO URL",
        )
    })
}

/// [`channel_slug`] of the checkout at `repo`.
pub fn workspace_channel_slug(repo: &Path) -> Result<String> {
    let path = repo.join("Cargo.toml");
    let cargo_text = fs::read_to_string(&path)
        .map_err(|error| Error::new(format!("read {}: {error}", path.display())))?;
    channel_slug(&cargo_text)
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
/// `[workspace.package] repository`. This runs before every real cut/recovery
/// mutation and is intentionally exact (GitHub may compare names
/// case-insensitively; the release protocol does not).
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

/// The release-org token on disk. Without it every call authenticates with
/// `gh auth token` (the dev account, with no push on the public channel), so the cut
/// refuses at [`preflight_channel_target`] before its claim. The publication engine
/// reads the same file (`pub`'s `MIRROR_TOKEN_PATH`).
pub(crate) fn channel_token_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".secrets/gh_access_token_alabsystems"))
}

/// Read + canonicalize the release-org token. `None` when absent, so a machine
/// without it falls back to `gh auth` and simply cannot publish.
pub(crate) fn channel_token() -> Option<String> {
    let token = fs::read_to_string(channel_token_path()?).ok()?;
    let token = token.trim().to_string();
    (!token.is_empty() && !token.bytes().any(|b| b.is_ascii_control())).then_some(token)
}

/// Is a channel-scoped credential in force for the current operation?
///
/// Set only by [`ChannelCred`], around work whose every `gh` call talks to the release
/// channel — every release-object call a real cut, a resume, a recovery, an abandon,
/// a yank, `status` and `verify` make. (The ledger, the lease, the fence and the tag
/// go through `git`, which never reads it.)
static CHANNEL_CRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// RAII scope: channel credential in force until dropped, including on the error
/// paths. Scopes nest — a yank's successor cut runs inside the yank's — so dropping
/// one restores what was in force when it was entered.
pub(crate) struct ChannelCred {
    previous: bool,
}

impl ChannelCred {
    pub(crate) fn enter() -> Self {
        Self {
            previous: CHANNEL_CRED.swap(true, Ordering::SeqCst),
        }
    }
}

impl Drop for ChannelCred {
    fn drop(&mut self) {
        CHANNEL_CRED.store(self.previous, Ordering::SeqCst);
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

/// Whether `args` asks `gh release download`, which reads an asset by its NAME: a
/// name can race a delete and re-upload and hand back another asset's bytes, so every
/// release asset read resolves the name to one id and reads that id
/// ([`exact_release_asset_download`]).
fn downloads_by_name(args: &[&str]) -> bool {
    args.windows(2).any(|w| w == ["release", "download"])
}

/// One `gh` invocation, captured. Spawn failure is an error; a non-zero exit
/// is returned to the caller (probes need to see "not found" exits). A name-based
/// asset download ([`downloads_by_name`]) is refused before `gh` runs.
pub fn gh_raw(args: &[&str]) -> Result<RunOut> {
    if downloads_by_name(args) {
        return Err(Error::new(format!(
            "refused `gh {}`: a release asset is read by its id, never by its name",
            args.join(" ")
        )));
    }
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

/// `gh` with success required, retried 3 times with backoff (2s, 5s): a mid-cut API
/// transient must not wedge a ten-minute build. Only idempotent operations come through
/// here (metadata edits, guarded deletes). Draft creation and asset upload do not: GitHub
/// may accept the POST and lose the response, so those persist one-shot intents.
pub fn gh_retry(args: &[&str]) -> Result<RunOut> {
    gh_retry_guarded(args, || Ok(()))
}

/// Mutation retry seam: revalidate the exact process fence immediately before
/// every attempt, including retries after a timeout/backoff.  A one-time step
/// entry check would let a rotated stale process wake and mutate later.
pub(crate) fn gh_retry_guarded(
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

/// Dedicated cooperative lock for every real release cut. A lightweight tag
/// points at the journaled claim commit, making ownership inspectable and
/// recoverable on another machine without a mutable lock payload.
pub const RELEASE_LEASE_REF: &str = "refs/tags/aterm-release-lease";

/// Per-invocation fencing token. The persistent lease points at the claim commit so
/// any machine can identify the cut, but that is not unique between two simultaneous
/// resumes. This ref points at a unique annotated-tag object peeling to the same claim,
/// giving each publisher process an exact compare-and-swap token.
pub const PUBLISHER_FENCE_REF: &str = "refs/tags/aterm-release-fence";

/// Mandatory acknowledgement for the one recovery operation whose safety has
/// an external, operator-established precondition. This is deliberately an
/// assertion, not a claim that the program can prove process quiescence.
pub const RECOVERY_STOPPED_PROCESS_FLAG: &str = "--old-publisher-stopped";

/// The operator's assertion that no draft was ever posted for this tag. A publisher
/// that died with its journal leaves `durable_create_intent` at `None`, and an absent
/// release object then means either no create POST was issued or one was issued and is
/// not yet visible; only a human at the releases page can tell. Separate from
/// [`RECOVERY_STOPPED_PROCESS_FLAG`], so the weaker claim is never made as a side effect
/// of the stronger. A journal that proves a POST was issued still wins: waiting is
/// then correct.
pub const RECOVERY_NO_DRAFT_POSTED_FLAG: &str = "--no-draft-was-posted";
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

// ---------------------------------------------------------------------------
// publisher liveness: who holds the fence, and can this machine prove they are gone?
// ---------------------------------------------------------------------------
//
// Two concurrent publishers would corrupt a release, and Git fencing cannot cancel an
// in-flight GitHub REST request. So the fence records who wrote it, and `--resume`
// reclaims a fence only under proof of death; an unprovable holder is refused. There
// is no time-based expiry (see `fence_liveness`).

/// Version of the `fence-*` identity block in the fence tag body. A fence with no
/// block parses to an empty [`FenceIdentity`], which is unprovable and never
/// auto-reclaimed. Format 2 adds `fence-pgid`: every GitHub mutation runs in a spawned
/// `gh` or `curl` child that a SIGKILL of the parent does not stop, so proof of death is
/// about the process group, and a format-1 fence whose pid is gone is unprovable.
pub const FENCE_IDENTITY_FORMAT: u32 = 2;

/// The version this process publishes, recorded into any fence it creates so a later
/// refusal on any machine can print the recover command filled in. Process-global like
/// the rest of the identity; set at cut/resume entry, before any fence exists.
static FENCE_SELF_VERSION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Record what this invocation publishes. Called once from `run_cut`, and again
/// from `resume_cut`, whose journaled version is the authoritative one.
pub fn set_publisher_fence_version(version: &str) {
    if let Ok(mut slot) = FENCE_SELF_VERSION.lock() {
        *slot = Some(version.trim().trim_start_matches('v').to_string());
    }
}

fn publisher_fence_self_version() -> Option<String> {
    FENCE_SELF_VERSION.lock().ok().and_then(|slot| slot.clone())
}

/// One recorded `fence-*` field value: single-line and bounded, so no recorded
/// value can forge another field or a whole extra line in the tag body.
fn fence_field(value: &str) -> String {
    let out: String = value
        .chars()
        .map(|c| if c.is_control() { '_' } else { c })
        .take(200)
        .collect();
    let out = out.trim().to_string();
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

/// The `fence-*` lines this process writes into a fence it creates: enough for any
/// reader, on any machine, to decide liveness and print the recover command. A field
/// the machine cannot answer is omitted, not guessed; a missing field is unprovable.
fn fence_identity_block(probe: &dyn PublisherProbe) -> String {
    let mut out = format!(
        "fence-format: {FENCE_IDENTITY_FORMAT}\nfence-pid: {}\n",
        std::process::id()
    );
    if let Some(pgid) = probe.pgid() {
        out.push_str(&format!("fence-pgid: {pgid}\n"));
    }
    for (key, value) in [
        ("host", probe.host()),
        ("boot", probe.boot()),
        ("exe", probe.exe()),
        ("version", publisher_fence_self_version()),
    ] {
        if let Some(value) = value {
            out.push_str(&format!("fence-{key}: {}\n", fence_field(&value)));
        }
    }
    if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
        out.push_str(&format!("fence-started: {}\n", now.as_secs()));
    }
    out
}

/// Who wrote a fence, as read back out of its annotated-tag body. Every field is
/// optional: a fence with no identity block must refuse safely, never panic or be
/// auto-reclaimed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FenceIdentity {
    pub format: Option<u32>,
    pub pid: Option<u32>,
    /// The publisher's process group, shared by every `gh`/`curl` it spawned. Absent
    /// on format-1 fences.
    pub pgid: Option<u32>,
    pub host: Option<String>,
    /// Boot-session identity. A pid is only meaningful within one boot, so this
    /// is as load-bearing as the pid itself.
    pub boot: Option<String>,
    pub exe: Option<String>,
    /// The release version that publisher was cutting, so a refusal can print
    /// `ship recover v<version> <claim>` without an operator looking it up.
    pub version: Option<String>,
    pub started_unix: Option<u64>,
    /// Set when the tag body could not be read at all (no network, object
    /// missing, malformed token). Distinct from "read, but carries no block".
    pub unreadable: Option<String>,
}

impl FenceIdentity {
    fn unreadable(reason: impl Into<String>) -> Self {
        FenceIdentity {
            unreadable: Some(reason.into()),
            ..FenceIdentity::default()
        }
    }

    /// Parse the `fence-*` lines out of a `git cat-file tag` body. Unknown
    /// keys, the tag headers and the human first line are all ignored; a
    /// malformed value leaves its field `None` — and therefore unprovable —
    /// never an error, because a refusal must survive any bytes it is handed.
    pub fn parse(body: &str) -> Self {
        let mut identity = FenceIdentity::default();
        for line in body.lines() {
            let Some(rest) = line.trim().strip_prefix("fence-") else {
                continue;
            };
            let Some((key, value)) = rest.split_once(':') else {
                continue;
            };
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            match key.trim() {
                "format" => identity.format = value.parse().ok(),
                "pid" => identity.pid = value.parse().ok(),
                "pgid" => identity.pgid = value.parse().ok(),
                "host" => identity.host = Some(value.to_string()),
                "boot" => identity.boot = Some(value.to_string()),
                "exe" => identity.exe = Some(value.to_string()),
                "version" => identity.version = Some(value.trim_start_matches('v').to_string()),
                "started" => identity.started_unix = value.parse().ok(),
                _ => {}
            }
        }
        identity
    }

    /// One human line naming the recorded publisher, for the refusal.
    pub fn describe(&self) -> String {
        if let Some(reason) = &self.unreadable {
            return format!("unreadable ({reason})");
        }
        if self.format.is_none() {
            return "not recorded — this fence predates publisher-liveness recording".to_string();
        }
        let mut parts = vec![match self.pid {
            Some(pid) => format!("pid {pid}"),
            None => "pid not recorded".to_string(),
        }];
        if let Some(pgid) = self.pgid {
            parts.push(format!("pgid {pgid}"));
        }
        if let Some(host) = &self.host {
            parts.push(format!("host {host}"));
        }
        if let Some(boot) = &self.boot {
            parts.push(format!("boot {boot}"));
        }
        if let Some(exe) = &self.exe {
            parts.push(format!("exe {exe}"));
        }
        if let Some(version) = &self.version {
            parts.push(format!("cutting v{version}"));
        }
        if let Some(started) = self.started_unix {
            parts.push(format!("fenced at unix {started}"));
        }
        parts.join(", ")
    }
}

/// What the local process table says about one recorded pid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PidState {
    Running {
        exe: Option<String>,
    },
    NotRunning,
    /// The probe itself failed. Never read as either answer.
    Unknown(String),
}

/// What the process table says about one recorded process group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupState {
    /// No live process carries this pgid: the publisher and everything it
    /// spawned are gone. The only answer that can license a reclaim.
    Empty,
    /// Live members, `(pid, command)`. A spawned `gh`/`curl` that outlived the
    /// publisher looks exactly like this — and so does a wrapper shell that is
    /// still waiting, which is the conservative direction: refuse, name it.
    Members(Vec<(u32, String)>),
    /// The probe itself failed. Never read as either answer.
    Unknown(String),
}

/// The machine facts liveness needs. A trait so the decision can be tested at
/// every verdict without arranging a real reboot, a second host, or a race with
/// the OS over pid reuse.
pub trait PublisherProbe {
    /// This machine's name.
    fn host(&self) -> Option<String>;
    /// This machine's current boot session. `None` ⇒ nothing is provable.
    fn boot(&self) -> Option<String>;
    /// This process's executable, for recording into a new fence.
    fn exe(&self) -> Option<String>;
    fn pid_state(&self, pid: u32) -> PidState;
    /// This process's own process group: recorded into a new fence, and used
    /// to refuse to judge a group this process is itself inside. `None` ⇒ the
    /// fence records no group and is unprovable to every reader.
    fn pgid(&self) -> Option<u32> {
        None
    }
    /// Every live process in a recorded group. The default is fail-closed: a
    /// probe that does not report groups cannot license a reclaim.
    fn group_members(&self, pgid: u32) -> GroupState {
        let _ = pgid;
        GroupState::Unknown("this probe does not report process groups".to_string())
    }
}

/// The real machine.
pub struct LocalProbe;

impl LocalProbe {
    fn uname_n() -> Option<String> {
        let out = Command::new("uname").arg("-n").output().ok()?;
        if !out.status.success() {
            return None;
        }
        let host = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!host.is_empty()).then_some(host)
    }

    /// A pid is only meaningful within one boot. Linux publishes a random UUID
    /// per boot; macOS/BSD publish the boot wall clock, whose seconds field is
    /// stable for the life of the boot. Either is enough to tell "the pid I
    /// recorded" from "a pid some later boot handed to someone else".
    fn boot_session() -> Option<String> {
        if let Ok(id) = fs::read_to_string("/proc/sys/kernel/random/boot_id") {
            let id = id.trim();
            if !id.is_empty() {
                return Some(format!("linux-boot-id:{id}"));
            }
        }
        let out = Command::new("sysctl")
            .args(["-n", "kern.boottime"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        // `{ sec = 1757000000, usec = 123456 } Mon Sep …` — take the seconds
        // field only; the trailing rendered date carries no extra identity and
        // is locale-shaped.
        let text = String::from_utf8_lossy(&out.stdout);
        let seconds: String = text
            .split("sec")
            .nth(1)?
            .trim_start()
            .trim_start_matches('=')
            .trim()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        (!seconds.is_empty()).then(|| format!("kern.boottime:{seconds}"))
    }

    /// Field 5 of a Linux `/proc/<pid>/stat` line is `pgrp`. The comm in
    /// field 2 is parenthesised and may itself contain spaces and parentheses,
    /// so the split is after the last `)`, never on whitespace from the start.
    fn pgrp_from_stat(stat: &str) -> Option<u32> {
        let (_, after) = stat.rsplit_once(')')?;
        // after: " S ppid pgrp session tty …"
        after.split_whitespace().nth(2)?.parse().ok()
    }

    fn own_pgid() -> Option<u32> {
        if let Ok(stat) = fs::read_to_string("/proc/self/stat") {
            return Self::pgrp_from_stat(&stat);
        }
        let out = Command::new("ps")
            .args(["-o", "pgid=", "-p", &std::process::id().to_string()])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }

    /// Every live process in `pgid`. Linux reads `/proc` directly; macOS/BSD
    /// reads the whole table once through `ps -A` and filters here. Either way
    /// the listing must contain this process, or it is a truncated table and
    /// not an answer — a cap on our own effort must never read as an empty
    /// group.
    fn members_of(pgid: u32) -> GroupState {
        let me = std::process::id();
        if Path::new("/proc/self/stat").exists() {
            let entries = match fs::read_dir("/proc") {
                Ok(entries) => entries,
                Err(error) => return GroupState::Unknown(format!("could not read /proc: {error}")),
            };
            let mut members = Vec::new();
            let mut saw_self = false;
            for entry in entries.flatten() {
                let Some(pid) = entry
                    .file_name()
                    .to_str()
                    .and_then(|s| s.parse::<u32>().ok())
                else {
                    continue;
                };
                // Exited between readdir and read: not a member.
                let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
                    continue;
                };
                saw_self |= pid == me;
                if Self::pgrp_from_stat(&stat) == Some(pgid) {
                    let comm = fs::read_to_string(entry.path().join("comm"))
                        .map(|c| c.trim().to_string())
                        .unwrap_or_default();
                    members.push((pid, comm));
                }
            }
            if !saw_self {
                return GroupState::Unknown("/proc did not list this process".to_string());
            }
            return if members.is_empty() {
                GroupState::Empty
            } else {
                GroupState::Members(members)
            };
        }
        let out = match Command::new("ps")
            .args(["-A", "-o", "pid=,pgid=,comm="])
            .output()
        {
            Ok(out) => out,
            Err(error) => return GroupState::Unknown(format!("could not run ps: {error}")),
        };
        if !out.status.success() {
            return GroupState::Unknown(format!(
                "ps -A exited {}: {}",
                out.status
                    .code()
                    .map_or_else(|| "by signal".to_string(), |code| code.to_string()),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut members = Vec::new();
        let mut saw_self = false;
        for line in text.lines() {
            let mut fields = line.split_whitespace();
            let (Some(pid), Some(group)) = (fields.next(), fields.next()) else {
                continue;
            };
            let Ok(pid) = pid.parse::<u32>() else {
                continue;
            };
            saw_self |= pid == me;
            if group.parse::<u32>().ok() == Some(pgid) {
                members.push((pid, fields.collect::<Vec<_>>().join(" ")));
            }
        }
        if !saw_self {
            return GroupState::Unknown("ps -A did not list this process".to_string());
        }
        if members.is_empty() {
            GroupState::Empty
        } else {
            GroupState::Members(members)
        }
    }
}

impl PublisherProbe for LocalProbe {
    fn pgid(&self) -> Option<u32> {
        LocalProbe::own_pgid()
    }

    fn group_members(&self, pgid: u32) -> GroupState {
        LocalProbe::members_of(pgid)
    }

    fn host(&self) -> Option<String> {
        static HOST: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        HOST.get_or_init(LocalProbe::uname_n).clone()
    }

    fn boot(&self) -> Option<String> {
        static BOOT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        BOOT.get_or_init(LocalProbe::boot_session).clone()
    }

    fn exe(&self) -> Option<String> {
        std::env::current_exe()
            .ok()
            .map(|path| path.display().to_string())
    }

    fn pid_state(&self, pid: u32) -> PidState {
        // Neither of these can be a publisher, and both are exactly what a
        // corrupt or truncated field parses to. Ambiguous ⇒ never an answer.
        if pid == 0 {
            return PidState::Unknown("pid 0 is not a real process id".to_string());
        }
        if pid == 1 {
            return PidState::Unknown("pid 1 is init, never a publisher".to_string());
        }
        // Linux: /proc is authoritative and needs no subprocess.
        if Path::new("/proc/self/stat").exists() {
            let dir = PathBuf::from(format!("/proc/{pid}"));
            if !dir.exists() {
                return PidState::NotRunning;
            }
            let exe = fs::read_link(dir.join("exe"))
                .ok()
                .map(|path| path.display().to_string())
                .or_else(|| {
                    fs::read_to_string(dir.join("comm"))
                        .ok()
                        .map(|name| name.trim().to_string())
                });
            return PidState::Running { exe };
        }
        // macOS/BSD: `ps -p` exits 1 for "no such process" and 0 with the command when
        // it exists. Any other exit is a probe failure, never an answer.
        match Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()
        {
            Err(error) => PidState::Unknown(format!("could not run ps: {error}")),
            Ok(out) if out.status.success() => {
                let exe = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if exe.is_empty() {
                    PidState::NotRunning
                } else {
                    PidState::Running { exe: Some(exe) }
                }
            }
            Ok(out) => {
                let complaint = String::from_utf8_lossy(&out.stderr).trim().to_string();
                match (out.status.code(), complaint.is_empty()) {
                    // The answer: `ps` ran, matched nothing, and said nothing.
                    (Some(1), true) => PidState::NotRunning,
                    // `ps` rejected the argument ("process id too large") or failed
                    // otherwise: a fact about the probe, never death.
                    (code, _) => PidState::Unknown(format!(
                        "ps -p {pid} exited {}: {complaint}",
                        code.map_or_else(|| "by signal".to_string(), |code| code.to_string())
                    )),
                }
            }
        }
    }
}

/// Do a recorded and an observed executable name describe the same program? Every
/// ambiguity answers `true` (alive, so refuse); only a confident mismatch answers
/// `false`, which licenses an automatic reclaim.
fn executables_agree(recorded: &str, observed: &str) -> bool {
    let base = |path: &str| {
        path.trim()
            .trim_end_matches(" (deleted)")
            .rsplit('/')
            .next()
            .unwrap_or(path)
            .trim()
            .to_string()
    };
    let recorded = base(recorded);
    let observed = base(observed);
    if recorded.is_empty() || observed.is_empty() || recorded == observed {
        return true;
    }
    // `/proc/<pid>/comm` truncates at 15 bytes and BSD `ps -o comm=` can be
    // clipped too: a long-enough prefix is a match, never a mismatch.
    let (short, long) = if recorded.len() <= observed.len() {
        (recorded.as_str(), observed.as_str())
    } else {
        (observed.as_str(), recorded.as_str())
    };
    short.len() >= 8 && long.starts_with(short)
}

/// Is the publisher that wrote this fence alive, dead, or unprovable?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceLiveness {
    /// Proved running. Refuse, always.
    Alive(String),
    /// Proved gone: same host, same boot session, and the pid is either absent
    /// from the process table or has been reused by a different program. A dead
    /// process cannot be racing, so and only so may `--resume` reclaim.
    Dead(String),
    /// Not proved either way. Refuse — an unprovable publisher might be racing.
    Unprovable(String),
}

impl FenceLiveness {
    pub fn label(&self) -> &'static str {
        match self {
            FenceLiveness::Alive(_) => "ALIVE",
            FenceLiveness::Dead(_) => "DEAD",
            FenceLiveness::Unprovable(_) => "UNPROVABLE",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            FenceLiveness::Alive(detail)
            | FenceLiveness::Dead(detail)
            | FenceLiveness::Unprovable(detail) => detail,
        }
    }
}

/// The liveness decision, pure over [`FenceIdentity`] + [`PublisherProbe`].
///
/// Only one path returns [`FenceLiveness::Dead`], and it needs all of: a readable
/// liveness block of a known format; a recorded pid, host and boot equal to this
/// machine's host and current boot (a hostname is not a unique machine identity, so a
/// different boot stays unprovable even though it usually means a reboot); a process
/// table answering "no such pid" or "a different program now"; and a recorded process
/// group the table says is empty. A live group member is [`FenceLiveness::Alive`];
/// anything else is [`FenceLiveness::Unprovable`].
///
/// No heartbeat, no expiry: an expiry would evict a holder still running, possibly with
/// a REST request in flight, and "wedged" is not separable from "slow". The refusal
/// names the pid instead: kill it, and the next `--resume` reclaims.
pub fn fence_liveness(identity: &FenceIdentity, probe: &dyn PublisherProbe) -> FenceLiveness {
    if let Some(reason) = &identity.unreadable {
        return FenceLiveness::Unprovable(format!(
            "the fence tag body could not be read: {reason}"
        ));
    }
    let Some(format) = identity.format else {
        return FenceLiveness::Unprovable(
            "the fence carries no liveness block — it was written by an aterm-release older \
             than fence-format 1, which recorded no pid, host or boot session"
                .to_string(),
        );
    };
    if format > FENCE_IDENTITY_FORMAT {
        return FenceLiveness::Unprovable(format!(
            "the fence is fence-format {format}, newer than this binary's \
             {FENCE_IDENTITY_FORMAT}; its fields may not mean what this binary would read"
        ));
    }
    let (Some(pid), Some(host), Some(boot)) = (
        identity.pid,
        identity.host.as_deref(),
        identity.boot.as_deref(),
    ) else {
        return FenceLiveness::Unprovable(
            "the fence's liveness block is incomplete — pid, host and boot session are all \
             required before a pid means anything"
                .to_string(),
        );
    };
    let Some(local_host) = probe.host() else {
        return FenceLiveness::Unprovable(
            "this machine could not determine its own hostname, so it cannot tell whether the \
             recorded pid belongs to it"
                .to_string(),
        );
    };
    let Some(local_boot) = probe.boot() else {
        return FenceLiveness::Unprovable(
            "this machine could not determine its own boot session, and a pid is only \
             meaningful within one boot"
                .to_string(),
        );
    };
    if !host.eq_ignore_ascii_case(&local_host) {
        return FenceLiveness::Unprovable(format!(
            "the fence was written on host {host}; this machine is {local_host}. A pid is only \
             meaningful on the host that issued it, and this process cannot see that machine's \
             process table"
        ));
    }
    if boot != local_boot {
        return FenceLiveness::Unprovable(format!(
            "the fence was written in boot session {boot}; this machine is now in {local_boot}. \
             The recorded pid cannot be that publisher any more, but a hostname is not a unique \
             machine identity, so this cannot separate \"this machine rebooted\" from \"another \
             machine with the same name wrote it\" — it is not proof of death"
        ));
    }
    // A running publisher is alive on this alone; a gone one is not yet dead.
    let publisher_gone = match probe.pid_state(pid) {
        PidState::NotRunning => format!(
            "pid {pid} is not in the process table of {host}, in the same boot session that \
             wrote the fence"
        ),
        PidState::Unknown(reason) => {
            return FenceLiveness::Unprovable(format!("pid {pid} could not be probed: {reason}"));
        }
        PidState::Running { exe: observed } => match (identity.exe.as_deref(), observed.as_deref())
        {
            (Some(recorded), Some(seen)) if !executables_agree(recorded, seen) => format!(
                "pid {pid} was reused — it now runs {seen}, not the recorded {recorded}, so \
                 the publisher that wrote the fence has exited"
            ),
            (_, seen) => {
                return FenceLiveness::Alive(format!(
                    "pid {pid} is running on {host}{}",
                    seen.map_or_else(String::new, |exe| format!(" as {exe}"))
                ));
            }
        },
    };
    // The spawned `gh`/`curl` children made the requests and share the group: only an
    // empty group proves nothing is still writing.
    let Some(pgid) = identity.pgid else {
        return FenceLiveness::Unprovable(format!(
            "{publisher_gone} — but the fence records no process group (fence-format {format}, \
             written before groups were recorded), and every GitHub request that publisher \
             made ran in a spawned gh or curl that a killed parent does not stop. Whether one \
             is still running cannot be read from this fence"
        ));
    };
    if probe.pgid() == Some(pgid) {
        return FenceLiveness::Unprovable(format!(
            "{publisher_gone} — but process group {pgid} is the one THIS process is in, and a \
             process cannot judge its own group empty"
        ));
    }
    match probe.group_members(pgid) {
        GroupState::Empty => FenceLiveness::Dead(format!(
            "{publisher_gone}, and process group {pgid} is empty: nothing it spawned is still \
             running"
        )),
        GroupState::Unknown(reason) => FenceLiveness::Unprovable(format!(
            "{publisher_gone}, but process group {pgid} could not be read: {reason}"
        )),
        GroupState::Members(members) => {
            let shown: Vec<String> = members
                .iter()
                .take(4)
                .map(|(member, command)| {
                    if command.is_empty() {
                        format!("pid {member}")
                    } else {
                        format!("pid {member} ({command})")
                    }
                })
                .collect();
            let more = members.len().saturating_sub(shown.len());
            let plural = members.len() != 1;
            FenceLiveness::Alive(format!(
                "{publisher_gone}, but its process group {pgid} still has {} live member{}: {}{} \
                 — a spawned GitHub request outlives the publisher that started it. Stop {} and \
                 the next --resume reclaims on its own",
                members.len(),
                if plural { "s" } else { "" },
                shown.join(", "),
                if more > 0 {
                    format!(" and {more} more")
                } else {
                    String::new()
                },
                if plural { "them" } else { "it" },
            ))
        }
    }
}

/// The exact recover command an operator must run, version and claim sha filled in.
pub fn fence_recover_command(version: Option<&str>, owner: &str) -> String {
    format!(
        "{SHIP_COMMAND} recover v{} {owner} {RECOVERY_STOPPED_PROCESS_FLAG}",
        version.unwrap_or("X.Y.Z")
    )
}

/// The self-diagnosing refusal. It names the holder, states the liveness verdict
/// and why, says what to do about that specific verdict, and ends with a
/// runnable recover command.
pub fn publisher_fence_refusal(
    fence: &PublisherFence,
    identity: &FenceIdentity,
    liveness: &FenceLiveness,
    fallback_version: Option<&str>,
) -> String {
    let (version, version_note) = match (identity.version.as_deref(), fallback_version) {
        (Some(version), _) => (Some(version), String::new()),
        (None, Some(version)) => (
            Some(version),
            format!(
                "\n  NOTE: the fence records no version (it predates fence-format \
                 {FENCE_IDENTITY_FORMAT}); v{version} is THIS invocation's version — confirm it \
                 against the stuck machine's dist/cut-state.toml before running the command"
            ),
        ),
        (None, None) => (
            None,
            "\n  NOTE: the fence records no version and this invocation has none; read \
             `version` from the stuck machine's dist/cut-state.toml and substitute it for X.Y.Z"
                .to_string(),
        ),
    };
    let advice = match liveness {
        FenceLiveness::Alive(_) => format!(
            "that publisher is STILL RUNNING. Do not steal the fence: a Git ref rotation cannot \
             cancel a GitHub REST request that is already in flight. Stop {} and run \
             `{CUT_COMMAND}` again — a resume RECLAIMS a fence whose publisher it can prove is dead, so \
             once that process is gone no recovery command is needed at all.",
            identity
                .pid
                .map_or_else(|| "it".to_string(), |pid| format!("pid {pid}"))
        ),
        FenceLiveness::Dead(_) => "that publisher is provably gone. `--resume` of THIS claim \
             reclaims the fence automatically; this path refused because the caller is not such \
             a resume — a fresh cut has no claim of its own to prove the fence is its own, and a \
             fence peeling to another claim is never touched."
            .to_string(),
        FenceLiveness::Unprovable(_) => "this machine CANNOT PROVE the publisher is gone, and an \
             unprovable publisher might still be racing, so the fence is kept. Establish by hand \
             that the old process exited, then run the command below."
            .to_string(),
    };
    format!(
        "publisher fence {PUBLISHER_FENCE_REF} is active at token {} for claim {}\n  \
         recorded publisher: {}\n  \
         liveness: {} — {}\n  \
         what to do: {advice}\n  \
         recover command (only after the old publisher is provably stopped):\n    {}{version_note}",
        fence.token,
        fence.owner,
        identity.describe(),
        liveness.label(),
        liveness.detail(),
        fence_recover_command(version, &fence.owner),
    )
}

/// Read the identity block out of the fence's annotated-tag object. Never fails: a body
/// that cannot be fetched or parsed becomes an `unreadable` identity, which refuses.
pub fn publisher_fence_identity(git: &dyn GitRunner, token: &str) -> FenceIdentity {
    if !valid_lease_owner(token) {
        return FenceIdentity::unreadable(format!("malformed fence token {token:?}"));
    }
    let local = git
        .git(&["cat-file", "-t", token])
        .map(|out| out.success() && out.stdout_utf8().trim() == "tag")
        .unwrap_or(false);
    if !local {
        // A fence written on another machine must be fetched first. `--no-tags` keeps
        // the probe from writing local refs; a failure falls through to the unreadable
        // identity below.
        let _ = git.git(&[
            "fetch",
            "--no-tags",
            "--quiet",
            "origin",
            PUBLISHER_FENCE_REF,
        ]);
    }
    match git.git(&["cat-file", "tag", token]) {
        Ok(out) if out.success() => FenceIdentity::parse(&out.stdout_utf8()),
        Ok(out) => FenceIdentity::unreadable(format!(
            "git cat-file tag {token} failed: {}",
            out.stderr_utf8().trim()
        )),
        Err(error) => FenceIdentity::unreadable(format!("git cat-file tag {token}: {error}")),
    }
}

/// Outcome of a resume's liveness probe against an active fence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceReclaim {
    /// Nothing held the fence.
    NoFence,
    /// The holder was proved dead and its exact token was CAS-deleted.
    Reclaimed { token: String, detail: String },
    /// The fence stays. `refusal` is the full self-diagnosing message the
    /// subsequent acquire will produce; callers print it as the reason.
    Kept { refusal: String },
}

/// `--resume`'s automatic reclaim: the only automatic path that removes another
/// process's fence. The fence is kept unless all hold:
///   * it peels to this resume's claim (another claim's fence is never touched);
///   * the release lease names the same claim (a disagreement is the recover lane's
///     to converge);
///   * [`fence_liveness`] returns [`FenceLiveness::Dead`].
///
/// The delete is an exact-token compare-and-swap, so two resumes that saw the same
/// dead fence produce one winner; the loser's create-only [`acquire_publisher_fence`]
/// then loses to the winner's fence, and its refusal names the winner's pid.
pub fn reclaim_dead_publisher_fence(
    git: &dyn GitRunner,
    expected_owner: &str,
    probe: &dyn PublisherProbe,
) -> Result<FenceReclaim> {
    let Some(fence) = publisher_fence(git)? else {
        return Ok(FenceReclaim::NoFence);
    };
    let identity = publisher_fence_identity(git, &fence.token);
    let liveness = fence_liveness(&identity, probe);
    let refusal = |extra: Option<&str>| {
        let base = publisher_fence_refusal(
            &fence,
            &identity,
            &liveness,
            publisher_fence_self_version().as_deref(),
        );
        match extra {
            Some(extra) => format!("{base}\n  reclaim: {extra}"),
            None => base,
        }
    };
    let expected_owner = expected_owner.to_ascii_lowercase();
    if !fence.owner.eq_ignore_ascii_case(&expected_owner) {
        return Ok(FenceReclaim::Kept {
            refusal: refusal(Some(
                "not reclaimable: the fence peels to a different claim than this resume's",
            )),
        });
    }
    if release_lease_owner(git)?.as_deref() != Some(expected_owner.as_str()) {
        return Ok(FenceReclaim::Kept {
            refusal: refusal(Some(
                "not reclaimable: the release lease does not name this claim, so the fence and \
                 the lease disagree — converge them with the recover command, not automatically",
            )),
        });
    }
    let FenceLiveness::Dead(detail) = &liveness else {
        return Ok(FenceReclaim::Kept {
            refusal: refusal(None),
        });
    };
    let lease_arg = format!("--force-with-lease={PUBLISHER_FENCE_REF}:{}", fence.token);
    let delete = format!(":{PUBLISHER_FENCE_REF}");
    let pushed = git.git(&["push", &lease_arg, "origin", &delete])?;
    match publisher_fence(git)? {
        None => Ok(FenceReclaim::Reclaimed {
            token: fence.token.clone(),
            detail: detail.clone(),
        }),
        // A rival resume won the same CAS: describe the current holder. The caller's
        // create-only acquire decides the rest.
        Some(current) if current.token != fence.token => {
            let identity = publisher_fence_identity(git, &current.token);
            let liveness = fence_liveness(&identity, probe);
            Ok(FenceReclaim::Kept {
                refusal: format!(
                    "{}\n  reclaim: not reclaimed: another session won the same \
                     compare-and-swap and now holds the fence",
                    publisher_fence_refusal(
                        &current,
                        &identity,
                        &liveness,
                        publisher_fence_self_version().as_deref(),
                    )
                ),
            })
        }
        Some(_) => Ok(FenceReclaim::Kept {
            refusal: refusal(Some(&format!(
                "not reclaimed: the exact-token delete did not land: {}",
                pushed.stderr_utf8().trim()
            ))),
        }),
    }
}

fn ensure_no_publisher_fence(git: &dyn GitRunner) -> Result<()> {
    if let Some(fence) = publisher_fence(git)? {
        let identity = publisher_fence_identity(git, &fence.token);
        let liveness = fence_liveness(&identity, &LocalProbe);
        return Err(Error::new(publisher_fence_refusal(
            &fence,
            &identity,
            &liveness,
            publisher_fence_self_version().as_deref(),
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
            "the release lock is held by claim {owner}; nothing was claimed. On the machine \
             that cut it: `{CUT_COMMAND}` or `--abandon vX.Y.Z`; from another machine, \
             once that cut is stopped: `{}`",
            fence_recover_command(None, &owner)
        )));
    }
    Ok(())
}

/// The cut's pre-claim gates that need nothing `pub publish` makes, asked by
/// `ship release` at its checks and again right before `pub publish`, so a refusal
/// (another machine's lease, a newer roster or floor on the channel) arrives while
/// nothing is public: no process run out of a staged bundle, the release lease, the
/// channel's write permission, the channel floor, the signing verdict, and the roster
/// lineage and ratchet. Returns every refusal as `(gate, message)`; the cut asks each
/// again before its claim. A credentials profile that does not load skips the signing
/// gates here: the provisioning audit's profile line names it. Not here: R's
/// commit-only gates, which read only R's tree (the release judges and journals them
/// at its step 4), the channel's source version (true only after `pub publish`), and
/// the atomic lease create.
pub fn pre_publish_gates(
    repo: &Path,
    version: &str,
    creds_path: &Path,
) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let git = GitCli::new(repo);
    if let Err(e) =
        gates::staged_bundle_liveness_gate(&repo.join("dist"), &crate::bundle::running_processes)
    {
        out.push(("staged bundle", e.to_string()));
    }
    if let Err(e) = preflight_release_lease(&git) {
        out.push(("release lease", e.to_string()));
    }
    let slug = match workspace_channel_slug(repo) {
        Ok(slug) => slug,
        Err(e) => {
            out.push(("channel", e.to_string()));
            return out;
        }
    };
    // every `gh` call below is a release-channel call, as the cut's are
    let _cred = ChannelCred::enter();
    if let Err(e) = preflight_channel_target(&slug, false) {
        out.push(("channel", e.to_string()));
    }
    let head_scan = match verify::scan_head_before_claim(&slug) {
        Ok(scan) => scan,
        Err(e) => {
            out.push(("channel floor", e.to_string()));
            return out;
        }
    };
    let newest_channel = head_scan.first();
    let newest_min_build = newest_channel.and_then(|published| published.min_build);
    let floor = ledger::show_origin_ledger(&git)
        .and_then(|text| ledger::tail(&text))
        .and_then(|tail| ledger::next_build(tail.build, unix_now()))
        .and_then(|provisional| effective_min_build(None, newest_min_build, provisional));
    if let Err(e) = floor {
        out.push(("channel floor", format!("v{version}: {e}")));
    }
    let Ok(Some(creds)) = sign::ReleaseCredentials::resolve(Some(creds_path), repo) else {
        return out;
    };
    let verdict = match preflight_signature_policy(Some(&creds), RosterDuty::Sign) {
        Ok(verdict) => verdict,
        Err(e) => {
            out.push(("signature", e.to_string()));
            return out;
        }
    };
    let ratchet = (|| -> Result<()> {
        let manifest_roster_seq = published_roster_seq(newest_channel)?;
        let observed_roster = machines::channel_roster_document(&slug).map_err(|e| {
            Error::new(format!(
                "cannot read the machine roster on the channel {slug}'s latest release ({e})"
            ))
        })?;
        let observed_roster_seq = observed_roster.as_ref().map(|(seq, _)| *seq);
        if let Some(document) = verdict.roster.as_ref() {
            machines::roster_lineage_agrees(
                &document.bytes,
                verdict.attribution.as_ref().map(|who| who.roster_seq),
                observed_roster.as_ref(),
            )
            .map_err(Error::new)?;
        }
        let newest_roster_seq = match (manifest_roster_seq, observed_roster_seq) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        roster_floor_covered(
            verdict.attribution.as_ref().map(|who| who.roster_seq),
            newest_roster_seq,
        )
    })();
    if let Err(e) = ratchet {
        out.push(("roster", e.to_string()));
    }
    out
}

/// Production `AcquireLease`: create-only push, followed by an authoritative
/// owner read. An existing exact owner is resume; a competing owner is refusal.
pub fn acquire_release_lease(
    git: &dyn GitRunner,
    expected_owner: &str,
) -> Result<ReleaseLeaseGuard> {
    // Cut and abandon both come through here; neither may enter while a prior process
    // fence survives. Only the recovery lane may rotate an observed exact token.
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
        "aterm publisher fence for claim {owner}; pid {}; nonce {nonce}; sequence {sequence}\n\n{}",
        std::process::id(),
        fence_identity_block(&LocalProbe),
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
    // The candidate ref is local scaffolding; its object stays available for the push
    // after the ref is removed.
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
/// each release mutation.
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

/// AN UNPUBLISHED CUT'S LAST TWO WRITES, in their one order: its tag is withdrawn
/// ([`delete_owned_release_tag`], the session re-proved first), and only then is the
/// owner released ([`release_completed_publisher_session`]) — an owner released first
/// would let a successor claim the version while this cut's tag still names it. Both
/// the lost-journal recovery and `--abandon` end through here.
///
/// # Errors
/// The session no longer this process's, or either write's own.
pub fn withdraw_tag_then_unlock(
    git: &dyn GitRunner,
    tag: &str,
    owner: &str,
    lease: &ReleaseLeaseGuard,
    fence: &PublisherFenceGuard,
) -> Result<LeaseRelease> {
    assert_publisher_session(git, lease, fence)?;
    delete_owned_release_tag(git, tag, owner, lease, fence)?;
    assert_publisher_session(git, lease, fence)?;
    release_completed_publisher_session(git, owner, fence)
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

/// CAS-safe unlock-only crash convergence, valid exclusively after every
/// publishing step is journaled. Deletion is permitted only with the exact
/// expected owner; an already-absent ref converges a crash after delete/before
/// journal mark, and a foreign create-only owner proves our ref was absent after
/// our prior CAS delete, so it is a successor, not a lease we may touch. Nothing
/// unlocks earlier: an interrupted cut keeps its lease and resumes as its owner.
pub fn release_completed_release_lease(
    git: &dyn GitRunner,
    expected_owner: &str,
) -> Result<LeaseRelease> {
    let expected_owner = expected_owner.to_ascii_lowercase();
    match release_lease_owner(git)? {
        None => return Ok(LeaseRelease::AlreadyAbsent),
        Some(owner) if owner != expected_owner => return Ok(LeaseRelease::AlreadySuperseded),
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

// ---------------------------------------------------------------------------
// the resume journal (dist/cut-state.toml)
// ---------------------------------------------------------------------------

/// Pipeline steps in execution order, as journaled. Gates and claim precede the journal
/// (a journal on disk means the claim is verified). `sign` covers
/// copy+sign+notarize+dmg+manifest as one re-enterable unit over the verified bundle
/// the release built once (`artifact.rs`); the cut builds nothing, so its outputs are
/// functions of `(version, build_number)` and those bytes. `publish` is the one
/// publication ([`step_publish`]). `unlock` is last: the website is `ship release`'s
/// after-cut step, so a site deploy can never park this journal.
pub const STEPS: [&str; 6] = ["lock", "sign", "selfcheck", "tag", "publish", "unlock"];

/// The one journal format this cutter reads and writes: exactly [`STEPS`], naming the
/// bytes it signs ([`Journal::artifact`], [`Journal::macho_sha256`]). A changed step
/// list gets a number no other cutter has written. Every entry point reads the
/// [`JournalHeader`] first: a finished journal of any format is history
/// ([`JournalHeader::finished`]), and an unfinished one is finished or withdrawn only by
/// the cutter of its claim ([`gates::own_cutter`]); [`Journal::load`] refuses another
/// format.
pub const JOURNAL_FORMAT: u32 = 12;

/// The part of a cut journal every format has written — the cut's version, build and
/// claim commit, and its completed steps — read before anything interprets a step, so
/// the cutter can decide who may act on a journal before it reads one it did not write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalHeader {
    /// The journal's format; the first format wrote none, and reads as 1.
    pub format: u32,
    pub version: String,
    pub build_number: u64,
    /// The claim commit — whose cutter finishes the cut.
    pub commit: String,
    /// Completed steps, as that format named them.
    pub done: Vec<String>,
}

impl JournalHeader {
    /// Read the header; `Ok(None)` when there is no journal. A file that has no version,
    /// build or claim commit is no cut journal at all, and is an error.
    pub fn read(path: &Path) -> Result<Option<Self>> {
        match read_journal_text(path)? {
            Some(text) => Self::parse(&text, path).map(Some),
            None => Ok(None),
        }
    }

    fn parse(text: &str, path: &Path) -> Result<Self> {
        #[derive(Deserialize)]
        struct Raw {
            format: Option<u32>,
            version: Option<String>,
            build_number: Option<u64>,
            commit: Option<String>,
            #[serde(default)]
            done: Vec<String>,
        }
        let raw: Raw = aterm_toml::from_str(text)
            .map_err(|e| Error::new(format!("parse {}: {e}", path.display())))?;
        let missing = |field: &str| {
            Error::new(format!(
                "{} names no {field}; it is not a cut journal",
                path.display()
            ))
        };
        Ok(Self {
            format: raw.format.unwrap_or(1),
            version: raw.version.ok_or_else(|| missing("version"))?,
            build_number: raw.build_number.ok_or_else(|| missing("build_number"))?,
            commit: raw.commit.ok_or_else(|| missing("claim commit"))?,
            done: raw.done,
        })
    }

    /// The cut released its lease and fence: `unlock` is journaled. Every format's step
    /// list ends at `unlock` or runs past it, so a finished journal of any format is
    /// history and asks nothing of any cutter.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.done.iter().any(|step| step == "unlock")
    }

    /// Where an unfinished cut stands, for a refusal: the step it re-enters at — or,
    /// for another format, whose steps this cutter does not read, its last journaled
    /// step and the cutter that finishes it.
    #[must_use]
    pub fn position(&self) -> String {
        if self.format == JOURNAL_FORMAT {
            let next = STEPS
                .iter()
                .find(|step| !self.done.iter().any(|done| done == *step))
                .unwrap_or(&"unlock");
            return format!("at step \"{next}\"");
        }
        format!(
            "after step \"{}\" by a format-{} cutter — only the cutter built at its claim {} \
             reads it",
            self.done.last().map_or("(none)", String::as_str),
            self.format,
            self.commit.get(..12).unwrap_or(&self.commit)
        )
    }
}

/// The journal file's text; `Ok(None)` when it is absent.
fn read_journal_text(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::new(format!("read {}: {e}", path.display()))),
    }
}

/// The cut journal — everything a re-entry (this machine or, together with
/// the remote-derived recut, any machine) needs to finish or abandon a cut.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    /// Always [`JOURNAL_FORMAT`]; any other value is refused on load.
    pub format: u32,
    /// Release version being cut, canonical `MAJOR.MINOR.PATCH` ("0.2.0").
    pub version: String,
    /// The verified ledger claim n.
    pub build_number: u64,
    /// The release commit (full sha): the published commit plus the claim's
    /// ledger line and changelog roll — artifacts come from exactly here, and the
    /// checkout sits on it, detached. It is on origin/main: as main's tip, or as
    /// the second parent of the merge main took.
    pub commit: String,
    /// Effective channel floor frozen at claim time: max(operator request,
    /// newest live manifest floor). Resume must restage the same manifest.
    #[serde(default)]
    pub min_build: Option<u64>,
    /// The bytes this cut signs: the release cache's key of the bundle the release
    /// built once and `verify release` judged (`artifact.rs`). `sign` copies exactly that
    /// entry, re-proved; a resume before `sign` copies it again and never builds.
    /// `None` only where nothing is left to sign (a recovered, already-published cut).
    #[serde(default)]
    pub artifact: Option<String>,
    /// The SHA-256 of that bundle's executable as `verify release` judged it — what its
    /// verified record names, and what the copy must still be before it is signed.
    #[serde(default)]
    pub macho_sha256: Option<String>,
    #[serde(default)]
    pub linux: Option<buildplan::linux::Handoff>,
    /// Whether this cut's uploaded channel manifest has a detached signature.
    /// Persisted so a resume re-proves the same signed appcast pair.
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
    /// The key the published artifacts must verify under, when that is not this
    /// machine's signing key: set only by a recovery, from the release's own
    /// master-roster-proven key (machine A published under Ka and died; machine B,
    /// signing with Kb, recovers it). `None` means [`Self::signature_pubkey`].
    #[serde(default)]
    pub verify_pubkey: Option<String>,
    /// Which machine signed, when the machine-roster tier is armed: the id the
    /// master-signed roster maps [`Self::signature_pubkey`] to. `None` with an unpinned
    /// paper master (a fork). Kept beside the key because the published manifest
    /// carries (and is signed over) this id, so a resume that re-authorizes to a
    /// different machine must abort. Public identity only.
    #[serde(default)]
    pub signature_machine_id: Option<String>,
    /// Immutable GitHub release object capability on the channel: the release this
    /// cut publishes onto. Draft tag names are not unique, so every upload, PATCH and
    /// delete after the bind is pinned to this ID and revalidates its tag and state.
    #[serde(default)]
    pub release_id: Option<u64>,
    /// Durable one-shot intent for the release itself: persisted before the
    /// non-idempotent create POST, or before an existing release under this tag is
    /// adopted (the engine's source prerelease — the object the intent then covers).
    /// If the response or the object's visibility is ambiguous, resume may discover
    /// the object but may never POST again.
    #[serde(default)]
    pub release_intent: bool,
    /// Exact asset names for which an upload POST has ever been issued. The
    /// set is append-only: an absent name after an ambiguous response may be
    /// eventual consistency, so resume must discover it rather than POSTing a
    /// duplicate object. It is also what `--abandon` withdraws from an adopted
    /// release: exactly the assets this cut put there.
    #[serde(default)]
    pub upload_intents: Vec<String>,
    /// Completed steps, in completion order (a subset of [`STEPS`]).
    #[serde(default)]
    pub done: Vec<String>,
}

impl Journal {
    /// Read this cutter's journal; `Ok(None)` when absent. Unparseable is an error (a
    /// half-written journal must stop resume, not restart a cut), and so is any format
    /// but [`JOURNAL_FORMAT`], read from the [`JournalHeader`] first so another cutter's
    /// journal gets a one-sentence refusal rather than a parse error.
    pub fn load(path: &Path) -> Result<Option<Journal>> {
        let Some(text) = read_journal_text(path)? else {
            return Ok(None);
        };
        let header = JournalHeader::parse(&text, path)?;
        if header.format != JOURNAL_FORMAT {
            return Err(Error::new(format!(
                "{} is a format-{} cut journal (v{} build {}, claim {}) and this cutter reads \
                 format {JOURNAL_FORMAT} only — the cutter built at that claim commit finishes \
                 or withdraws it, and a finished one is history the next cut clears",
                path.display(),
                header.format,
                header.version,
                header.build_number,
                header.commit,
            )));
        }
        let journal: Journal = aterm_toml::from_str(&text)
            .map_err(|e| Error::new(format!("parse {}: {e}", path.display())))?;
        journal.validate()?;
        Ok(Some(journal))
    }

    /// This cutter's journal, or `Ok(None)` for no journal and for another cutter's
    /// format: for callers that only read a journal (`verify`, `yank`'s key lookup, the
    /// release cache's key list), which a journal they cannot read must not fail.
    pub fn load_if_ours(path: &Path) -> Result<Option<Journal>> {
        match JournalHeader::read(path)? {
            Some(header) if header.format == JOURNAL_FORMAT => Self::load(path),
            _ => Ok(None),
        }
    }

    /// Persist atomically (temp + rename): a crash mid-write must never leave
    /// a torn journal that blocks its own recovery path.
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let text = aterm_toml::to_string(self)
            .map_err(|e| Error::new(format!("serialize journal: {e}")))?;
        // dist/ (git-ignored) may not exist yet: the first save happens as soon as the
        // claim is verified, and a fresh clone (spec §5 recut) has none. Failing here
        // would burn the just-pushed ledger number.
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
        crate::durable::sync_all(&file)
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
            crate::durable::sync_dir(parent).map_err(|e| {
                Error::new(format!(
                    "fsync journal parent directory {}: {e}",
                    parent.display()
                ))
            })?;
        }
        // In a fresh clone, syncing dist/ is not enough: its own entry must survive in
        // the repository directory. Sync each newly created directory's parent.
        for created in newly_created_dirs {
            let parent = created.parent().ok_or_else(|| {
                Error::new(format!(
                    "new journal directory {} has no parent to fsync",
                    created.display()
                ))
            })?;
            crate::durable::sync_dir(parent).map_err(|e| {
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

    /// A corrupt or stale journal must not become an authority for an impossible
    /// manifest during resume. New journals are canonicalized by
    /// [`effective_min_build`]; this also catches a hand-edited one.
    fn validate(&self) -> Result<()> {
        if self.format != JOURNAL_FORMAT {
            return Err(Error::new(format!(
                "release journal format {} is not this cutter's format {JOURNAL_FORMAT}",
                self.format
            )));
        }
        ledger::check_version_shape(&self.version)
            .map_err(|error| Error::new(format!("release journal has invalid version: {error}")))?;
        if !valid_lease_owner(&self.commit) {
            return Err(Error::new(
                "release journal commit is not a full 40- or 64-hex claim object id",
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
                "release journal done list is not an exact known, unique, ordered, gap-free \
                 prefix of the canonical pipeline",
            ));
        }
        if let Some(linux) = &self.linux {
            linux.validate_identity(
                &self.version,
                self.build_number,
                &self.commit,
                self.is_done("sign"),
            )?;
        }
        validate_min_build(self.min_build, self.build_number, "journaled build")?;
        // Until `sign` is journaled the cut has bytes left to sign, and they are the
        // verified ones by name: a journal that cannot name them would have a resume
        // sign something else — or build, which a claimed release never does.
        if !self.is_done("sign") {
            let named = |value: &Option<String>| {
                value.as_deref().is_some_and(|hex| {
                    hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                })
            };
            if !named(&self.artifact) || !named(&self.macho_sha256) {
                return Err(Error::new(
                    "release journal has not signed yet and names no verified bundle (artifact \
                     key + executable sha256) to sign",
                ));
            }
        }
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
        if self.is_done("sign") && self.signature_required && !self.manifest_signed {
            return Err(Error::new(
                "release journal marks sign complete without its required manifest signature",
            ));
        }
        // The release capability: an object ID implies the durable intent that bound
        // it, and upload intents imply both. A journal that failed these could
        // authorize a second POST against the channel the whole fleet reads.
        if self.release_id == Some(0) {
            return Err(Error::new("release journal carries a zero release ID"));
        }
        if self.is_done("publish") && self.release_id.is_none() {
            return Err(Error::new(
                "release journal marks publish complete without an immutable GitHub release ID",
            ));
        }
        if self.release_id.is_some() && !self.release_intent {
            return Err(Error::new(
                "release journal carries an immutable release ID without its durable release intent",
            ));
        }
        let mut intents = std::collections::BTreeSet::new();
        if self.upload_intents.iter().any(|name| {
            name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
                || !intents.insert(name)
        }) {
            return Err(Error::new(
                "release journal upload intents are empty, non-canonical, or duplicated",
            ));
        }
        if !self.upload_intents.is_empty() && (self.release_id.is_none() || !self.release_intent) {
            return Err(Error::new(
                "release journal carries upload intents without its bound release",
            ));
        }
        Ok(())
    }

    /// The first [`STEPS`] entry not yet journaled — where `--resume` re-enters.
    /// `None` ⇒ the cut completed.
    pub fn first_incomplete(&self) -> Option<&'static str> {
        STEPS.iter().copied().find(|step| !self.is_done(step))
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
// pure publish helpers (tested in tests/it/resume.rs)
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

/// An absent listing is destructive-cleanup authority only when a current durable
/// journal proves no create POST was issued, or the operator answers for a lost journal
/// (`None`) with [`RECOVERY_NO_DRAFT_POSTED_FLAG`]. `Some(true)` is never overridable:
/// delayed visibility is then the only explanation, and waiting is correct.
#[must_use]
pub const fn absent_draft_decision(
    durable_create_intent: Option<bool>,
    operator_asserts_no_post: bool,
) -> AbsentDraftDecision {
    match (durable_create_intent, operator_asserts_no_post) {
        (Some(false), _) | (None, true) => AbsentDraftDecision::AbandonProvenNoPost,
        (Some(true), _) | (None, false) => AbsentDraftDecision::RetainOwnerAwaitVisibility,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftCleanupDecision {
    AbandonProvenNoPost,
    DeleteIssuedVisible,
    RetainIssuedAwaitVisibility,
    RefuseUnknownOrInconsistent,
}

/// `claim_bound_draft` means the visible draft under this tag carries nothing but
/// assets this version publishes ([`channel::binds_to_version`]): a channel release has
/// no claim-commit target to bind by, so it binds by what it holds, and a draft holding
/// anything else is someone else's object.
///
/// A recovery on a second machine always has `None` intent (the journal died with the
/// publisher). When the remote binds the draft to this claim that adds nothing: only
/// the lease holder writes this tag's release, the lease names this claim, and
/// `recover` requires [`RECOVERY_STOPPED_PROCESS_FLAG`]. An unbound draft still refuses.
#[must_use]
pub const fn draft_cleanup_decision(
    durable_create_intent: Option<bool>,
    exact_draft_visible: bool,
    claim_bound_draft: bool,
) -> DraftCleanupDecision {
    match (
        durable_create_intent,
        exact_draft_visible,
        claim_bound_draft,
    ) {
        (Some(false), false, _) => DraftCleanupDecision::AbandonProvenNoPost,
        (Some(true), true, _) => DraftCleanupDecision::DeleteIssuedVisible,
        (Some(true), false, _) => DraftCleanupDecision::RetainIssuedAwaitVisibility,
        (None, true, true) => DraftCleanupDecision::DeleteIssuedVisible,
        (Some(false), true, _) | (None, false, _) | (None, true, false) => {
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
        "--upload-file",
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
    // Under a channel scope the upload targets the public channel, which `gh auth`
    // cannot write; the header file then carries the release-org token.
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
/// construction, before the caller persists its durable intent; the
/// permit-consuming [`Self::issue`] then goes straight to curl. The held temp
/// dirs keep the payload and header files alive until the POST returns.
struct OneShotPost {
    _payload_dir: Option<PrivateTempDir>,
    _auth: GithubAuthHeaders,
    args: Vec<String>,
}

/// curl's exit for "I could not even start": argument and initialisation failures,
/// which happen strictly before any connection is attempted.
const CURL_EXIT_FAILED_INIT: i32 = 2;

/// Did this attempt provably not reach the network? The one-shot design assumes the
/// server saw every request and never repeats one; this is the one case knowable
/// locally. curl exits 2 when it rejects its own arguments or fails to initialise,
/// before connect(2). A timeout, a reset, a 5xx or a killed process does not qualify:
/// each can hide a delivered request.
const fn transport_never_started(out: &RunOut) -> bool {
    out.status == CURL_EXIT_FAILED_INIT
}

/// curl's exit for "the server answered, and its answer was an HTTP error"
/// (`--fail`). The request was delivered and processed; only the response was a
/// refusal.
const CURL_EXIT_HTTP_ERROR: i32 = 22;

/// Did the server refuse this request with a 4xx, so it created nothing? The sibling
/// of [`transport_never_started`]: a later invocation may post again. If the object
/// does not exist the retry makes it; if it does (a `422 already_exists` not yet
/// visible) the retry is refused the same way, so neither can mint a duplicate. A
/// draft-create POST that races GitHub's convergence on a just-pushed release commit is
/// answered `422`, and must leave the cut able to retry or abandon.
///
/// 4xx only: a 5xx can hide a write applied before the server fell over, and a proxy's
/// 502/504 can hide a delivered request; both keep the conservative reading.
fn server_refused_without_creating(out: &RunOut) -> bool {
    if out.status != CURL_EXIT_HTTP_ERROR {
        return false;
    }
    http_status_from_curl_failure(&out.stderr_utf8()).is_some_and(|code| (400..500).contains(&code))
}

/// The status code out of curl's `--fail` diagnostic
/// (`The requested URL returned error: <code>`). An unrecognised message reads as
/// unknown, never as a refusal.
fn http_status_from_curl_failure(stderr: &str) -> Option<u16> {
    let tail = stderr.rsplit_once("returned error: ")?.1;
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// What a failed one-shot POST proves about the object it asked for — the two local,
/// provable facts ([`transport_never_started`], [`server_refused_without_creating`])
/// and everything else, in the one place every caller of them reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PostOutcome {
    /// curl refused its own arguments: nothing left this machine.
    NeverSent,
    /// The server answered with a 4xx: it understood the request and created nothing.
    Refused,
    /// A 5xx, a timeout, a reset, a killed process, a curl message this code has never
    /// seen — or a success. Each can hide a delivered request, so the intent stands.
    Unknown,
}

/// Classify one finished one-shot POST. Pure, so the table is a test.
pub(crate) fn post_outcome(out: &RunOut) -> PostOutcome {
    if transport_never_started(out) {
        PostOutcome::NeverSent
    } else if server_refused_without_creating(out) {
        PostOutcome::Refused
    } else {
        PostOutcome::Unknown
    }
}

/// What `upload_channel_asset` does next, once its POST has finished and
/// [`CutCtx::settle_upload_intent`] has spent (or kept) the intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AfterUploadPost {
    /// curl never sent it and nothing of that name is visible: fail, naming the local
    /// cause. The intent is retracted.
    NeverSent,
    /// The server refused it and nothing of that name is visible: fail, with GitHub's
    /// words. The intent is retracted.
    Refused,
    /// An object of this name is visible: prove its bytes against `dist/` and converge
    /// on it, never POST again. `journal` is true when the intent was retracted first (a
    /// refused POST), so the journal must name the object again, or `--abandon` cannot
    /// withdraw it.
    Converge { journal: bool },
    /// The outcome is unknowable and nothing is visible: fail without a retry. The
    /// intent stands, and a resume waits for the object.
    Ambiguous,
}

/// Decide [`AfterUploadPost`] from what the POST proved ([`PostOutcome`], which `settle`
/// returns once it has retracted or kept the intent) and a probe for the exact-name
/// asset. Pure over both, so the table is a test, and the fake GitHub in
/// `tests/it/channel_latest.rs` reads this one function instead of copying it.
///
/// THE SETTLE COMES FIRST, BY CONSTRUCTION: `settle` runs before `visible` is asked at
/// all, so the intent's local verdict never waits on a network probe that can fail
/// under exactly the conditions that produce a never-sent POST (a failed probe that
/// left the intent standing is the permanent wedge the retraction exists to prevent).
///
/// * Never sent asks nothing: the probe can fail under the conditions that produce it.
/// * Refused probes once, best-effort: `422 already_exists` is a 4xx too, and an
///   unjournaled object would sit beyond `--abandon`'s reach. A failed probe leaves
///   the refusal standing.
/// * Unknown probes, and a failed probe fails the step; the intent stands.
///
/// # Errors
/// The settle's own (the probe is then never asked), and the probe's, for an unknown
/// outcome only.
pub(crate) fn after_upload_post(
    settle: impl FnOnce() -> Result<PostOutcome>,
    visible: impl FnOnce() -> Result<bool>,
) -> Result<AfterUploadPost> {
    let outcome = settle()?;
    Ok(match outcome {
        PostOutcome::NeverSent => AfterUploadPost::NeverSent,
        PostOutcome::Refused => {
            if visible().unwrap_or(false) {
                AfterUploadPost::Converge { journal: true }
            } else {
                AfterUploadPost::Refused
            }
        }
        PostOutcome::Unknown => {
            if visible()? {
                AfterUploadPost::Converge { journal: false }
            } else {
                AfterUploadPost::Ambiguous
            }
        }
    })
}

/// GitHub's "I understood you and I refuse the content" — the code a release
/// body over the limit comes back as, and the one a reader cannot decode from
/// the number alone.
const HTTP_UNPROCESSABLE_CONTENT: u16 = 422;

/// GitHub's own refusal document, on the failing POST's stdout (`--fail-with-body`):
/// `{"message": "...", "errors": [{...}]}`. Only the fields that name the problem are
/// read; a response of another shape reads as "GitHub said nothing", not an error.
#[derive(Deserialize, Default)]
struct GithubRefusal {
    #[serde(default)]
    message: String,
    #[serde(default)]
    errors: Vec<GithubRefusalDetail>,
}

#[derive(Deserialize, Default)]
struct GithubRefusalDetail {
    #[serde(default)]
    resource: String,
    #[serde(default)]
    field: String,
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

/// GitHub's refusal in its own words, or `None` when the response carried none.
fn github_refusal_summary(response: &[u8]) -> Option<String> {
    let refusal: GithubRefusal = aterm_json::from_slice(response).ok()?;
    let fields: Vec<String> = refusal
        .errors
        .iter()
        .map(|detail| {
            let mut named = String::new();
            if !detail.resource.is_empty() {
                named.push_str(&detail.resource);
            }
            if !detail.field.is_empty() {
                if !named.is_empty() {
                    named.push('.');
                }
                named.push_str(&detail.field);
            }
            let said = if detail.message.is_empty() {
                detail.code.clone()
            } else {
                detail.message.clone()
            };
            match (named.is_empty(), said.is_empty()) {
                (true, true) => "unnamed error".to_string(),
                (true, false) => said,
                (false, true) => named,
                (false, false) => format!("{named}: {said}"),
            }
        })
        .collect();
    match (refusal.message.is_empty(), fields.is_empty()) {
        (true, true) => None,
        (false, true) => Some(refusal.message),
        (true, false) => Some(fields.join("; ")),
        (false, false) => Some(format!("{} ({})", refusal.message, fields.join("; "))),
    }
}

/// What the failing release POST said, in place of curl's bare
/// `curl: (22) … returned error: 422`. A 422 means GitHub refused the content, so this
/// adds the posted body's size against the release-body limit (either way) and whatever
/// GitHub named in its own `errors[]`. Every other failure passes through as curl
/// reported it.
fn release_post_failure_detail(out: &RunOut, posted_body_len: usize) -> String {
    let mut detail = out.stderr_utf8().trim().to_string();
    if http_status_from_curl_failure(&out.stderr_utf8()) == Some(HTTP_UNPROCESSABLE_CONTENT) {
        let limit = changelog::GITHUB_RELEASE_BODY_LIMIT;
        detail.push_str(&format!(
            " — {HTTP_UNPROCESSABLE_CONTENT} is GitHub understanding the request and refusing \
             its CONTENT. The release body this POST carried was {posted_body_len} bytes against \
             the {limit}-character release-body limit, which is "
        ));
        if posted_body_len > limit {
            detail.push_str(
                "OVER IT — that is this refusal, and nothing else needs investigating until \
                 the body fits",
            );
        } else {
            detail.push_str(
                "WITHIN it — so the body's size is not the cause here; the tag, the target \
                 commitish and GitHub's own words are what is left to read",
            );
        }
    }
    if let Some(said) = github_refusal_summary(&out.stdout) {
        detail.push_str(&format!(" — GitHub said: {said}"));
    }
    detail
}

/// **THE RELEASE BODY, READ AND BOUNDED AT THE POST ITSELF.**
///
/// [`changelog::release_notes_document`] bounds what it writes, but the file read
/// here can come from an older binary, a resumed cut, or an operator's hand; each
/// would still post over the limit as an opaque 422. Guard the OPERATION.
///
/// Idempotent: a body already within the limit is returned verbatim.
///
/// The one constructor of [`ReleaseBody`], the only body [`release_create_payload`]
/// takes: a release body is posted bounded by construction.
fn release_body_for_post(label: &str, path: &Path) -> Result<ReleaseBody> {
    let raw = fs::read_to_string(path)
        .map_err(|error| Error::new(format!("read {label} release notes: {error}")))?;
    let bounded = changelog::bound_release_body(&raw);
    if bounded.len() != raw.len() {
        step(
            label,
            &format!(
                "{} is {} bytes, over the {} GitHub accepts in a release body; posting it cut \
                 at a section boundary with a pointer to CHANGELOG.md for the rest (an \
                 unbounded body is refused as an opaque 422)",
                path.display(),
                raw.len(),
                changelog::GITHUB_RELEASE_BODY_LIMIT,
            ),
        );
    }
    Ok(ReleaseBody(bounded))
}

/// A release body read and bounded at the post ([`release_body_for_post`], its only
/// constructor outside tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReleaseBody(String);

impl std::ops::Deref for ReleaseBody {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

/// The release-create request: a draft of `tag` named `title`, its body `body`, which
/// only [`release_body_for_post`] makes.
///
/// # Errors
/// The request does not serialize.
fn release_create_payload(tag: &str, title: &str, body: &ReleaseBody) -> Result<Vec<u8>> {
    aterm_json::to_vec(&aterm_json::json!({
        "tag_name": tag,
        "name": title,
        "body": &**body,
        "draft": true,
        "prerelease": false,
    }))
    .map_err(|error| Error::new(format!("serialize release request: {error}")))
}

/// How curl is told to find the request body: buffered costs memory equal to the
/// payload, streamed a constant (see [`OneShotPost::prepare_binary`]).
#[derive(Clone, Copy)]
pub(crate) enum BodySource<'a> {
    /// `--data-binary @path`: read fully into memory first. Small JSON only.
    Buffered(&'a str),
    /// `--upload-file path`: streamed off disk, any size.
    Streamed(&'a str),
}

impl<'a> BodySource<'a> {
    pub(crate) const fn curl_pair(self) -> (&'static str, &'a str) {
        match self {
            Self::Buffered(arg) => ("--data-binary", arg),
            Self::Streamed(path) => ("--upload-file", path),
        }
    }
}

impl OneShotPost {
    /// JSON-body POST (the channel draft's create). `temp_label` names the temp
    /// directory; `subject` names the request in errors.
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
            args: Self::curl_args(
                &auth.curl_header_arg,
                "Content-Type: application/json",
                // A draft-create body is a few hundred bytes; buffering it is free.
                BodySource::Buffered(&data_arg),
                endpoint,
            ),
            _payload_dir: Some(payload_dir),
            _auth: auth,
        })
    }

    /// Raw file-body POST (asset uploads). `subject` names the file in errors.
    ///
    /// Streamed, never buffered: `--data-binary @file` reads the whole payload into
    /// memory, and the batteries-included DMG is over a gigabyte. `--upload-file`
    /// streams it with a `Content-Length` from the file's size; `--request POST`
    /// overrides its default PUT, and the endpoint ends in a query string, not `/`, so
    /// curl appends no file name.
    fn prepare_binary(subject: &str, endpoint: &str, file: &Path) -> Result<Self> {
        let file_arg = file
            .to_str()
            .ok_or_else(|| Error::new(format!("{subject} path is not UTF-8")))?;
        let auth = prepare_github_auth_headers()?;
        Ok(Self {
            args: Self::curl_args(
                &auth.curl_header_arg,
                "Content-Type: application/octet-stream",
                BodySource::Streamed(file_arg),
                endpoint,
            ),
            _payload_dir: None,
            _auth: auth,
        })
    }

    /// Takes the header argument, not the `GithubAuthHeaders` that owns it: the
    /// argv this builds is the whole security- and memory-relevant surface of a
    /// one-shot POST, and a test must be able to inspect it without a token.
    fn curl_args(
        auth_header_arg: &str,
        content_type: &str,
        body: BodySource<'_>,
        endpoint: &str,
    ) -> Vec<String> {
        let (body_flag, body_arg) = body.curl_pair();
        [
            "--silent",
            "--show-error",
            "--fail-with-body",
            "--retry",
            "0",
            "--request",
            "POST",
            "--header",
            auth_header_arg,
            "--header",
            content_type,
            body_flag,
            body_arg,
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

/// Canonical release-channel floor. Zero means the same as absence, so a cut
/// never emits `min_build = 0`.
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

/// The roster ratchet, sibling of [`channel_floor_covered`]: a cut may not publish a
/// roster generation older than the channel head's. The client ratchets `roster_seq` on
/// observation and `Roster::admit` returns `Rollback` below it, before any artifact
/// crypto, with no fallback to an older release; `machines::authorize_cut` cannot see
/// that remote mark. A stale copy is the normal case (the roster in `dist/` is
/// git-ignored and hand-copied), and it is a security property: a machine revoked in a
/// newer generation is still authorized by its own older copy.
///
/// `None` on the channel side (an unarmed fork) is `Ok`. An unattributed cut against a
/// rostered head is refused: the client refuses that downgrade structurally.
pub fn roster_floor_covered(carried: Option<u64>, newest_channel: Option<u64>) -> Result<()> {
    // `>=`, not `>`: the client's floor is the head's own `roster_seq`, and a second
    // machine holding the same roster republishes at that generation.
    match (carried, newest_channel) {
        (_, None) => Ok(()),
        (Some(carried), Some(newest)) if carried >= newest => Ok(()),
        (Some(carried), Some(newest)) => Err(Error::new(format!(
            "this machine's roster is older than the channel's, so publishing would stop \
             every up-to-date client from updating at all.\n\
             \n\
             channel head  machine roster generation {newest}\n\
             this cut      generation {carried}\n\
             fix           refresh this machine's copy of aterm-machines.toml — the \
             master-signed document `atpkg-keys join` / `machine-revoke` wrote — and cut \
             again\n\
             \n\
             why: every client that has already seen generation {newest} refuses a release \
             under an older one (RosterReject::Rollback) BEFORE it checks any artifact \
             crypto, and the updater has no fallback to an older release."
        ))),
        (None, Some(newest)) => Err(Error::new(format!(
            "the channel head was published under machine roster generation {newest}, but \
             this cut carries no attribution at all; an armed client refuses a release with \
             no aterm-machines.toml structurally. Cut from a machine the roster lists, or \
             unpin the paper master in a tracked commit"
        ))),
    }
}

// ---------------------------------------------------------------------------
// the channel's appcast listing, for the head-signature replay
// ---------------------------------------------------------------------------

/// One GitHub release asset relevant to the head-signature replay.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AppcastAsset {
    pub id: u64,
    pub name: String,
}

/// The appcast assets on one release. Drafts remain represented so the replay can
/// prove they are skipped rather than relying on the API query to hide them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AppcastRelease {
    pub release_id: u64,
    pub tag: String,
    pub draft: bool,
    pub target_commitish: String,
    pub assets: Vec<AppcastAsset>,
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
            "release {} has duplicate assets named {name}; refusing an ambiguous head",
            release.tag
        )));
    }
    Ok(first)
}

/// What one published release tag is to the current version protocol. The
/// publisher's classification is the client's (`aterm-update/src/github.rs`):
/// both compile the one grammar in [`aterm_update_core::tag`], so publisher and
/// fleet cannot disagree about which releases are even candidates.
pub use aterm_update_core::tag::TagKind;

/// Classify one release tag by [`aterm_update_core::tag::parse_release_tag`]; only the
/// diagnostic wording is here. Canonical `vMAJOR.MINOR.PATCH` is a candidate, exactly
/// two components is the retired scheme ([`TagKind::Legacy`]), and anything else is a
/// hard error, so garbage in the tag namespace fails closed.
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
/// A retired two-component tag is not canonical authority — callers that must
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

const APPCAST_ASSET_LIST_JQ: &str = r#".[] | . as $r |
    {release_id: $r.id,
     tag: $r.tag_name,
     draft: $r.draft,
     target_commitish: $r.target_commitish,
     assets: [$r.assets[]? |
       select(.name == "aterm-appcast.toml" or .name == "aterm-appcast.toml.sig") |
       {id: .id, name: .name}]}
    | @json"#;

/// Parse the bounded GitHub listing [`list_appcast_releases`] reads.
/// Each line represents one release even when it has no relevant assets, so
/// pagination counts releases rather than assets.
pub fn parse_appcast_asset_listing(listing: &str) -> Result<Vec<AppcastRelease>> {
    let mut releases = Vec::new();
    for (index, line) in listing.lines().enumerate() {
        let release: AppcastRelease = aterm_json::from_str(line).map_err(|error| {
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

/// Every release on `slug` with its exact appcast pair, from a bounded listing.
fn list_appcast_releases(slug: &str) -> Result<Vec<AppcastRelease>> {
    const PER_PAGE: usize = 100;
    const MAX_PAGES: u32 = 10;
    let mut releases = Vec::new();
    for page in 1..=MAX_PAGES {
        let path = format!("repos/{slug}/releases?per_page={PER_PAGE}&page={page}");
        let out = gh_retry(&["api", &path, "--jq", APPCAST_ASSET_LIST_JQ])?;
        let page_releases = parse_appcast_asset_listing(&out.stdout_utf8())?;
        let page_len = page_releases.len();
        releases.extend(page_releases);
        if page_len < PER_PAGE {
            break;
        }
        if page == MAX_PAGES {
            return Err(Error::new(format!(
                "GitHub release listing reached the {MAX_PAGES}-page safety cap"
            )));
        }
    }
    Ok(releases)
}

// ---------------------------------------------------------------------------
// cryptographic channel ratchet + exact asset reads
// ---------------------------------------------------------------------------

fn update_key_fingerprint(encoded: &str) -> Result<String> {
    let canonical = canonical_update_pubkey(encoded)?;
    let raw = aterm_codec::base64::decode_strict(canonical.as_bytes())
        .map_err(|_| Error::new("canonical update key failed to decode for fingerprint"))?;
    Ok(sha256_bytes(&raw))
}

/// The cut's signing verdict: whether it signs, and with which key. Public as the
/// integration-test seam for the decision table ([`channel_signature_policy`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignaturePolicy {
    pub required: bool,
    pub pubkey: Option<String>,
}

/// The verdict of a tree with no paper master pinned (a fork): signing is per-machine
/// opt-in. Configured material signs; a keyless machine cuts unsigned (Tier REPO). No
/// client of such a tree verifies a signature, so nothing here can strand one.
pub fn unrostered_signature_policy(material_pubkey: Option<&str>) -> Result<SignaturePolicy> {
    Ok(match material_pubkey {
        Some(pubkey) => SignaturePolicy {
            required: true,
            pubkey: Some(canonical_update_pubkey(pubkey)?),
        },
        None => SignaturePolicy {
            required: false,
            pubkey: None,
        },
    })
}

/// What a pipeline entry will still do with the signing material, and so how much of
/// the roster chain it re-proves. A cut that will still sign is choosing an
/// attribution and must prove the roster still authorizes this machine. A cut
/// finishing already-signed bytes has no choice left (the attribution is inside a
/// signature, the roster frozen in `dist/`): re-proving could only fail spuriously,
/// strand a cut one upload from done when a roster window lapses, and refuse
/// cross-machine recovery. Same trade as [`resume_apple_tier`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterDuty {
    /// This entry may still produce and sign new manifest bytes. The full chain runs.
    Sign,
    /// Every byte this entry will publish is already assembled and signed. The key
    /// decision runs; the roster chain does not.
    Finish,
}

/// Everything the armed machine-roster tier decides on, bundled so a caller cannot
/// supply four parameters and forget the fifth; the shape of the client's
/// `RosterPolicy`. Empty `master_pubkeys` is the whole two-state switch: the tier is
/// absent, and [`channel_signature_policy`] is [`unrostered_signature_policy`].
pub struct RosterEvidence<'a> {
    /// The pinned paper master(s) — `pins::PAPER_MASTER_PUBKEYS` in production.
    pub master_pubkeys: &'a [&'a str],
    /// The master-signed roster this cut claims authority from, read once,
    /// pre-claim. `None` means the profile named none — which is a refusal on the
    /// armed path, never a downgrade.
    pub roster: Option<&'a machines::RosterDocument>,
    /// What this machine claims its id is (profile, else `~/.aterm/machine.toml`).
    /// A cross-check only; see [`machines::declared_machine_id`].
    pub declared_machine_id: Option<&'a str>,
    /// Injected wall clock, so every freshness case is testable without waiting.
    pub now_unix: i64,
    /// Whether this entry can still sign, which is what decides whether the roster
    /// chain is any of its business. See [`RosterDuty`].
    pub duty: RosterDuty,
}

/// The two-state signing gate, chosen by the paper master anchor alone.
///
/// Anchor empty (a fork): [`unrostered_signature_policy`], per-machine opt-in, no
/// attribution.
///
/// Anchor armed: the roster governs alone. The client's `fetch_authoritative_release`
/// (aterm-update's github.rs) consults only the master-signed roster under an armed
/// anchor, so this gate asks what a client will: does the roster name this key's
/// machine, unrevoked and in its window? No client falls back to a compiled-in key.
/// Every armed failure is a refusal.
pub fn channel_signature_policy(
    material_pubkey: Option<&str>,
    evidence: &RosterEvidence<'_>,
) -> Result<(SignaturePolicy, Option<roster::Attribution>)> {
    if evidence.master_pubkeys.is_empty() {
        return Ok((unrostered_signature_policy(material_pubkey)?, None));
    }
    let Some(material) = material_pubkey else {
        return Err(Error::new(
            "no signing material, and the release channel takes only a rostered machine's \
             key: cut with --release-credentials ~/.aterm/release-credentials.toml (`targo \
             --unverified ship provision --id <id>` writes it)",
        ));
    };
    let material = canonical_update_pubkey(material)?;
    // A finish entry stops here: nothing left to sign, so no roster question (see
    // [`RosterDuty`]). `None` attribution stops a caller comparing a fresh local
    // claim against bytes that already shipped.
    if evidence.duty == RosterDuty::Finish {
        return Ok((
            SignaturePolicy {
                required: true,
                pubkey: Some(material),
            },
            None,
        ));
    }
    let Some(document) = evidence.roster else {
        return Err(Error::new(
            "the signing profile names no `machine_roster`: add `machine_roster = \
             \"<path>/aterm-machines.toml\"` (its .sig beside it), or move the profile aside \
             and re-run `targo --unverified ship provision --id <id>`, which writes it",
        ));
    };
    let who = machines::authorize_cut(
        evidence.master_pubkeys,
        document.bytes.clone(),
        &document.signature,
        &material,
        evidence.now_unix,
    )?;
    // The cross-check runs last: the roster has already decided whose key this is. A
    // profile (or `~/.aterm/machine.toml`) that disagrees means a copied profile, a
    // re-minted machine or mixed-up keys, each of which would publish an attribution
    // true of the bytes and false of the world.
    if let Some(declared) = evidence.declared_machine_id
        && declared != who.machine_id
    {
        // The alternative to correcting the declaration is cutting with the declared
        // machine's own key — offered only when the roster actually names that machine.
        let alternative = match machines::roster_pubkey_for(
            evidence.master_pubkeys,
            document.bytes.clone(),
            &document.signature,
            declared,
        ) {
            Some(_) => format!(", or cut with the key that belongs to {declared:?}"),
            None => String::new(),
        };
        return Err(Error::new(format!(
            "this machine declares it is {declared:?}, but the roster maps the configured \
             signing key to {:?}. Refusing to publish an attribution that contradicts the \
             machine it was cut on — set `machine_id = {:?}` in the release-credentials \
             profile, which is what a cut from this machine under that key is{alternative}",
            who.machine_id, who.machine_id,
        )));
    }
    Ok((
        SignaturePolicy {
            required: true,
            pubkey: Some(material),
        },
        Some(who),
    ))
}

/// Decode and re-emit the updater Ed25519 key so journal/config comparisons
/// use one canonical identity rather than textual base64 aliases.
///
/// The key arrives as an argument — from the signing material, the release
/// journal, or the machine roster — never from the environment.
pub fn canonical_update_pubkey(encoded: &str) -> Result<String> {
    let encoded = encoded.trim();
    let bytes = aterm_codec::base64::decode_strict(encoded.as_bytes())
        .map_err(|_| Error::new("updater signing key is not valid standard base64"))?;
    if bytes.len() != 32 {
        return Err(Error::new(format!(
            "updater signing key decodes to {} bytes, not an Ed25519 32-byte public key",
            bytes.len()
        )));
    }
    aterm_codec::base64::encode(&bytes)
        .map_err(|_| Error::new("updater signing key is too large to re-encode"))
}

/// Verify raw detached Ed25519 bytes against the canonical/persisted channel
/// key.  This is the same primitive the pinned updater uses.
pub fn verify_detached_manifest_signature(
    encoded_pubkey: &str,
    manifest: &[u8],
    signature: &[u8],
) -> Result<()> {
    let canonical = canonical_update_pubkey(encoded_pubkey)?;
    let pubkey = aterm_codec::base64::decode_strict(canonical.as_bytes())
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

/// The exact-head signature check, with the asset download injected so the decision
/// runs without GitHub: exactly one published `head_tag` carrying the exact manifest
/// and signature, the signature byte-identical to the local cut artifact when one is
/// given, and valid for `head_manifest` under `pubkey`.
pub fn verify_channel_head_signature_with(
    releases: &[AppcastRelease],
    head_tag: &str,
    head_manifest: &[u8],
    local_head_signature: Option<&[u8]>,
    pubkey: &str,
    fetch_asset: impl FnOnce(u64, u64, &str, &str) -> Result<Vec<u8>>,
) -> Result<()> {
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
    let signature_id =
        unique_asset_id(head, manifest_out::MANIFEST_SIG_ASSET)?.ok_or_else(|| {
            Error::new(format!(
                "signed channel head {head_tag} has no exact {}",
                manifest_out::MANIFEST_SIG_ASSET
            ))
        })?;
    let head_signature = fetch_asset(
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
    verify_detached_manifest_signature(pubkey, head_manifest, &head_signature).map_err(|error| {
        Error::new(format!(
            "signed channel head {head_tag} is invalid under the configured public key: {error}"
        ))
    })
}

/// Live wrapper used by `targo --unverified ship verify` and a yank's successor proof.
/// With no configured or journaled update key the channel is unsigned (Tier REPO) and
/// published signature history never forces a signed successor. With a key, the exact
/// live head signature is checked by [`verify_channel_head_signature_with`] over a
/// snapshot-bound download.
pub fn verify_live_channel_head_signature(
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
    let releases = list_appcast_releases(slug)?;
    verify_channel_head_signature_with(
        &releases,
        head_tag,
        head_manifest,
        local_head_signature,
        &pubkey,
        |release_id, asset_id, tag, name| {
            download_snapshot_appcast_asset(slug, &releases, release_id, asset_id, tag, name)
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

/// The loaded signing identity. The key is held in memory by
/// [`sign::ReleaseCredentials`], loaded once from `--release-credentials`, and
/// signing happens in-process (docs/RELEASE-KEYS.md).
struct SigningMaterial {
    pubkey: String,
}

/// Derive the signing identity from the credentials supplied on the command line.
///
/// `None` means no `--release-credentials` was given — legal only for an unpinned
/// channel, which `channel_signature_policy` decides, not this function.
/// Nothing here reads the filesystem or the environment: whether a machine can cut
/// is a property of the command that ran, not of ambient state.
fn load_signing_material(
    creds: Option<&sign::ReleaseCredentials>,
) -> Result<Option<SigningMaterial>> {
    let Some(creds) = creds else {
        return Ok(None);
    };
    Ok(Some(SigningMaterial {
        pubkey: canonical_update_pubkey(creds.pubkey())?,
    }))
}

/// Resolve Tier APPLE from the anchor: `pins::anchor_active` is the predicate, so an
/// unpinned build is inert. The anchor is a parameter, not a read: only [`run_cut`] and
/// [`resume_cut`] pass `pins::APPLE_TEAM_ID` in, so a test drives every rule with a
/// placeholder team.
///
/// The anchor is a PARAMETER, not a read: only [`run_cut`] and [`resume_cut`] name
/// `pins::APPLE_TEAM_ID` to decide anything, so a test can drive every rule below
/// with a placeholder team instead of the armed real one.
///
/// Deliberately NOT varied by [`CutKind`]: a dry run signs and notarizes for real.
/// Skipping the most failure-prone step would rehearse the easy part, and `spctl`
/// rejects a never-notarized bundle at the self-check anyway.
fn resolve_apple_tier(
    team_id: &str,
    credentials: Option<&sign::ReleaseCredentials>,
) -> Result<sign::AppleTier> {
    if !aterm_update_core::pins::anchor_active(team_id) {
        return Ok(sign::AppleTier::Inactive);
    }
    let tier = sign::resolve_apple_tier(team_id, credentials).map_err(Error::new)?;
    // Announced only when active: the inactive tier adds zero transcript lines.
    println!("{}", tier.describe());
    Ok(tier)
}

/// The tier a resume must resolve: nothing unless `sign` will still run. Only
/// [`step_sign`] reads `ctx.apple`; every later step re-proves the artifacts on disk
/// against the manifest's `team_id` ([`selfcheck_signing`]), so demanding a certificate
/// past `sign` would only make a cut unfinishable when it expires between signing and
/// upload. When `sign` will run this resolves as [`run_cut`] does, by the same
/// predicate as the signing-key re-proof beside its call site.
pub fn resume_apple_tier(
    team_id: &str,
    journal: &Journal,
    credentials: Option<&sign::ReleaseCredentials>,
) -> Result<sign::AppleTier> {
    if journal.is_done("sign") {
        // Not a claim that the tier is off: nothing remaining will sign or notarize,
        // and `Inactive` makes every hook a no-op if one were somehow reached.
        return Ok(sign::AppleTier::Inactive);
    }
    resolve_apple_tier(team_id, credentials)
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
    let response: Response = aterm_json::from_slice(bytes)
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

/// One point-in-time answer to both halves of a download-bracket end.
#[derive(Debug)]
pub struct ReleaseObjectAndAsset {
    /// `None` only when the release object itself is absent (HTTP 404), exactly
    /// as [`release_object_by_id`] reports it.
    pub release: Option<ReleaseObjectIdentity>,
    /// `(asset id, size)`, or `None` when the release carries no asset with the
    /// requested exact name — the same answer
    /// [`release_asset_identity_for_release_id_optional`] gives.
    pub asset: Option<(u64, u64)>,
}

/// The fused bracket read's jq. Each row is tagged with its kind so the existing
/// parsers keep their row shapes: the fields after `R` match
/// [`RELEASE_IDENTITY_OBJECT_JQ`] and those after `A` the asset-identity program of
/// [`release_asset_identity_for_release_id_optional`]. A release with no assets yields
/// zero `A` rows: "no such asset".
const RELEASE_OBJECT_AND_ASSETS_JQ: &str = r#"(["R", .id, .tag_name,
      (.draft | tostring), .target_commitish] | @tsv),
    (.assets[]? | ["A", .name, (.id | tostring), (.size | tostring)] | @tsv)"#;

/// Split a [`RELEASE_OBJECT_AND_ASSETS_JQ`] response into the two row blocks its tags
/// name, stripping the tag so each block is exactly what its parser expects. An
/// untagged or unknown row fails closed, never degrading into "no assets".
fn split_release_object_and_asset_rows(rows: &str) -> Result<(String, String)> {
    let mut object_rows = String::new();
    let mut asset_rows = String::new();
    for (index, line) in rows.lines().enumerate() {
        let (kind, fields) = line.split_once('\t').ok_or_else(|| {
            Error::new(format!("malformed fused GitHub release row {}", index + 1))
        })?;
        let block = match kind {
            "R" => &mut object_rows,
            "A" => &mut asset_rows,
            _ => {
                return Err(Error::new(format!(
                    "fused GitHub release row {} has unknown kind {kind:?}",
                    index + 1
                )));
            }
        };
        block.push_str(fields);
        block.push('\n');
    }
    Ok((object_rows, asset_rows))
}

/// The release-object identity and the exact-name asset binding from one read of
/// `repos/{slug}/releases/{id}`, so each bracket end is a single point-in-time snapshot
/// (two reads would miss a mutation landing between them). The caller checks the result.
pub fn release_object_and_asset_identity(
    slug: &str,
    release_id: u64,
    name: &str,
) -> Result<ReleaseObjectAndAsset> {
    let endpoint = format!("repos/{slug}/releases/{release_id}");
    let args = [
        "api",
        endpoint.as_str(),
        "--jq",
        RELEASE_OBJECT_AND_ASSETS_JQ,
    ];
    // A 404 is an answer, not a failure, so it must not burn the retry budget
    // (`release_object_by_id`'s rule). Any other non-zero exit retries.
    let out = gh_raw(&args)?;
    let out = if out.success() {
        out
    } else {
        let stderr = out.stderr_utf8();
        if stderr.contains("HTTP 404") || stderr.contains("Not Found") {
            return Ok(ReleaseObjectAndAsset {
                release: None,
                asset: None,
            });
        }
        gh_retry(&args)?
    };
    let (object_rows, asset_rows) = split_release_object_and_asset_rows(&out.stdout_utf8())?;
    let rows = parse_release_object_identity_rows(&object_rows)?;
    let [identity] = rows.as_slice() else {
        return Err(Error::new(format!(
            "exact GitHub release ID {release_id} returned {} identity rows",
            rows.len()
        )));
    };
    if identity.id != release_id {
        return Err(Error::new(format!(
            "exact GitHub release endpoint {release_id} returned foreign ID {}",
            identity.id
        )));
    }
    let asset =
        parse_release_asset_identity_rows(&asset_rows, &format!("release-ID:{release_id}"), name)?;
    Ok(ReleaseObjectAndAsset {
        release: Some(identity.clone()),
        asset,
    })
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
    // A streaming download spawns its own child rather than going through `gh_raw`, so
    // the channel credential is threaded in here: without it a read inside a
    // `ChannelCred` scope falls back to the dev account and 404s on the channel.
    if let Some(token) = active_channel_token() {
        command.env("GH_TOKEN", token);
    }
    command
        .spawn()
        .map_err(|error| Error::new(format!("spawn exact GitHub asset-ID download: {error}")))
}

// ---------------------------------------------------------------------------
// asset transfer retry
//
// `gh_retry` repeats on any non-zero exit, so it cannot tell a lost body (`gh`:
// "unexpected end of JSON input") from a server that answered "no". A 404, a 401
// and a permission refusal are answers; only a transport failure can come out
// differently the second time.
// ---------------------------------------------------------------------------

/// Whether repeating a bounded asset transfer can possibly change its outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AssetTransferFault {
    /// The bytes did not arrive intact — a truncated body, a reset socket, a
    /// name that would not resolve, a 5xx. The request never got an answer, so
    /// asking again is the remedy.
    Transport,
    /// The server answered and the answer was no — 404, 401/403, a bound the
    /// asset genuinely exceeds, `gh` not on PATH. Repeating this re-asks a
    /// question that has already been decided.
    Answered,
}

/// One failed attempt: its class, and the sentence a human needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TransferAttemptFailure {
    pub(crate) fault: AssetTransferFault,
    pub(crate) message: String,
}

impl TransferAttemptFailure {
    pub(crate) fn transport(message: impl Into<String>) -> Self {
        Self {
            fault: AssetTransferFault::Transport,
            message: message.into(),
        }
    }
    pub(crate) fn answered(message: impl Into<String>) -> Self {
        Self {
            fault: AssetTransferFault::Answered,
            message: message.into(),
        }
    }
}

/// Sleeps between attempts (so one more attempt than entries): the same 2s/5s shape
/// as `gh_retry`, long enough to outlast a blip, short enough to report an outage.
pub(crate) const ASSET_TRANSFER_BACKOFFS: &[u64] = &[2, 5];

/// Decide, from a `gh` diagnostic, whether the transport failed or the server
/// answered. Unrecognised is `Answered`: defaulting to `Transport` would make every
/// novel failure cost a full backoff ladder and report it under the wrong label, so a
/// new transport symptom belongs in the table. Answered markers are tested first
/// because a 404 body can carry transport-ish wording.
#[must_use]
pub(crate) fn classify_asset_transfer_failure(diagnostic: &str) -> AssetTransferFault {
    let d = diagnostic.to_ascii_lowercase();
    const ANSWERED: &[&str] = &[
        "http 401",
        "http 403",
        "http 404",
        "http 410",
        "http 422",
        "not found",
        "bad credentials",
        "requires authentication",
        "resource not accessible",
        "must have admin rights",
        "forbidden",
        "permission",
        "gh auth login",
        "no such release",
        "saml enforcement",
    ];
    const TRANSPORT: &[&str] = &[
        // `gh` read a body that stopped mid-object.
        "unexpected end of json input",
        "unexpected eof",
        "unexpected end of stream",
        "connection reset",
        "connection refused",
        "connection closed",
        "broken pipe",
        "i/o timeout",
        "timed out",
        "timeout",
        "tls handshake",
        "handshake failure",
        "temporary failure in name resolution",
        "no such host",
        "network is unreachable",
        "network is down",
        "http 500",
        "http 502",
        "http 503",
        "http 504",
        "server error",
        "goaway",
        "stream error",
    ];
    if ANSWERED.iter().any(|m| d.contains(m)) {
        return AssetTransferFault::Answered;
    }
    if TRANSPORT.iter().any(|m| d.contains(m)) {
        return AssetTransferFault::Transport;
    }
    AssetTransferFault::Answered
}

/// The bounded retry, with the transfer injected so the loop is testable without a
/// network. The final message names the class the failure was judged to be.
pub(crate) fn retry_transport_failures<T>(
    what: &str,
    backoffs: &[u64],
    sleep: &mut dyn FnMut(u64),
    mut attempt: impl FnMut(u32) -> std::result::Result<T, TransferAttemptFailure>,
) -> Result<T> {
    let attempts = backoffs.len() + 1;
    let mut last = String::new();
    for index in 0..attempts {
        let n = u32::try_from(index).unwrap_or(u32::MAX).saturating_add(1);
        match attempt(n) {
            Ok(value) => return Ok(value),
            Err(failure) => {
                last = failure.message;
                if failure.fault == AssetTransferFault::Answered {
                    return Err(Error::new(format!(
                        "{what} failed: {last} — the server answered, so this is not \
                         a transport fault and was NOT retried"
                    )));
                }
                if let Some(backoff) = backoffs.get(index) {
                    eprintln!(
                        "    {what} hit a transport fault (attempt {n}/{attempts}): \
                         {last} — retrying in {backoff}s"
                    );
                    sleep(*backoff);
                }
            }
        }
    }
    Err(Error::new(format!(
        "{what} failed after {attempts} attempts: {last} — a transport fault that \
         did not clear; `{CUT_COMMAND}` re-enters here without rebuilding"
    )))
}

/// One bounded transfer of the asset with the given immutable ID, classified.
fn attempt_exact_release_asset_transfer(
    slug: &str,
    id: u64,
    expected_size: u64,
    limit: usize,
) -> std::result::Result<Vec<u8>, TransferAttemptFailure> {
    let mut child = exact_release_asset_download(slug, id)
        .map_err(|error| TransferAttemptFailure::answered(error.to_string()))?;
    let stdout = child.stdout.take().ok_or_else(|| {
        TransferAttemptFailure::answered("exact GitHub asset-ID download has no stdout pipe")
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        TransferAttemptFailure::answered("exact GitHub asset-ID download has no stderr pipe")
    })?;
    let stderr_reader = std::thread::spawn(move || drain_bounded_diagnostic(stderr, 64 * 1024));
    let bytes = match read_bounded_release_asset(stdout, limit) {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stderr_reader.join();
            let text = error.to_string();
            // A read that died is transport; a read that ran out of bound is an
            // answer about the asset, and repeating it returns the same bytes.
            return Err(if text.contains("exceeded its") {
                TransferAttemptFailure::answered(text)
            } else {
                TransferAttemptFailure::transport(text)
            });
        }
    };
    let status = child.wait().map_err(|error| {
        TransferAttemptFailure::answered(format!(
            "wait for exact GitHub asset-ID download: {error}"
        ))
    })?;
    let (stderr, stderr_truncated) = match stderr_reader.join() {
        Ok(Ok(pair)) => pair,
        Ok(Err(error)) => return Err(TransferAttemptFailure::transport(error.to_string())),
        Err(_) => {
            return Err(TransferAttemptFailure::answered(
                "exact GitHub asset-ID stderr reader panicked",
            ));
        }
    };
    if !status.success() {
        let diagnostic = format!(
            "{}{}",
            String::from_utf8_lossy(&stderr).trim(),
            if stderr_truncated {
                " [diagnostic truncated at 65536 bytes]"
            } else {
                ""
            }
        );
        return Err(TransferAttemptFailure {
            fault: classify_asset_transfer_failure(&diagnostic),
            message: diagnostic,
        });
    }
    let downloaded_size = u64::try_from(bytes.len()).map_err(|_| {
        TransferAttemptFailure::answered("downloaded release-asset length does not fit u64")
    })?;
    if downloaded_size != expected_size {
        // A body that ended early but exited 0. Same fault, quieter symptom.
        return Err(TransferAttemptFailure::transport(format!(
            "API size {expected_size} differs from bounded download size {downloaded_size}"
        )));
    }
    Ok(bytes)
}

#[cfg(test)]
mod asset_transport_retry_tests {
    //! The retry loop with the transfer injected: how often it asks, how long it
    //! waits, and which failures it declines to repeat.

    use super::*;
    use std::cell::RefCell;

    /// `gh`'s stderr for a truncated body.
    const TRUNCATED: &str = "unexpected end of JSON input";

    fn run(
        script: Vec<std::result::Result<&'static str, TransferAttemptFailure>>,
    ) -> (Result<&'static str>, Vec<u32>, Vec<u64>) {
        let script = RefCell::new(script.into_iter());
        let seen = RefCell::new(Vec::new());
        let slept = RefCell::new(Vec::new());
        let out = retry_transport_failures(
            "download exact release asset aterm-appcast.toml from alabsystems/aterm",
            ASSET_TRANSFER_BACKOFFS,
            &mut |s| slept.borrow_mut().push(s),
            |n| {
                seen.borrow_mut().push(n);
                script
                    .borrow_mut()
                    .next()
                    .expect("the loop asked more times than the script allows")
            },
        );
        (out, seen.into_inner(), slept.into_inner())
    }

    #[test]
    fn the_truncated_body_that_killed_two_cuts_is_a_transport_fault_and_a_404_is_not() {
        assert_eq!(
            classify_asset_transfer_failure(TRUNCATED),
            AssetTransferFault::Transport,
            "the measured killer must be the retried class, or this fix is inert"
        );
        for answered in [
            "gh: Not Found (HTTP 404)",
            "HTTP 403: Resource not accessible by integration",
            "gh: Bad credentials (HTTP 401)",
            "To get started with GitHub CLI, please run: gh auth login",
        ] {
            assert_eq!(
                classify_asset_transfer_failure(answered),
                AssetTransferFault::Answered,
                "{answered}"
            );
        }
        for transport in [
            "read tcp 10.0.0.2:443: connection reset by peer",
            "Post \"https://api.github.com\": net/http: TLS handshake timeout",
            "gh: Server Error (HTTP 502)",
        ] {
            assert_eq!(
                classify_asset_transfer_failure(transport),
                AssetTransferFault::Transport,
                "{transport}"
            );
        }
        // An unrecognised diagnostic is reported, not ground through the whole
        // ladder wearing a label nobody established.
        assert_eq!(
            classify_asset_transfer_failure("something nobody has seen yet"),
            AssetTransferFault::Answered
        );
    }

    #[test]
    fn an_injected_truncated_response_is_retried_with_backoff_and_the_cut_survives() {
        let (out, seen, slept) = run(vec![
            Err(TransferAttemptFailure::transport(TRUNCATED)),
            Ok("appcast bytes"),
        ]);
        assert_eq!(
            out.expect(
                "a transport hiccup must not end a cut that has already \
                        built, signed, notarized and stapled"
            ),
            "appcast bytes"
        );
        assert_eq!(seen, vec![1, 2], "the second ask is what saves the cut");
        assert_eq!(slept, vec![2], "and it waits before it");
    }

    #[test]
    fn a_truncated_response_that_never_clears_is_bounded_and_names_its_class() {
        let (out, seen, slept) = run(vec![
            Err(TransferAttemptFailure::transport(TRUNCATED)),
            Err(TransferAttemptFailure::transport(TRUNCATED)),
            Err(TransferAttemptFailure::transport(TRUNCATED)),
        ]);
        out.expect_err("three truncated bodies is a failure");
        assert_eq!(seen, vec![1, 2, 3]);
        assert_eq!(slept, ASSET_TRANSFER_BACKOFFS.to_vec());
    }

    #[test]
    fn a_real_404_fails_fast_and_says_it_was_not_retried() {
        let (out, seen, slept) = run(vec![Err(TransferAttemptFailure::answered(
            "gh: Not Found (HTTP 404)",
        ))]);
        out.expect_err("a 404 is a failure");
        assert_eq!(
            seen,
            vec![1],
            "re-asking for an asset that is not there burns seven seconds and \
             then reports the 404 as an exhausted retry"
        );
        assert!(slept.is_empty(), "and it must not wait to do it");
    }
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

/// The bracketed transfer. Crate-visible so the authoritative scan can hand in a
/// recheck that reads the asset binding and the release-object identity in one call
/// ([`release_object_and_asset_identity`]).
pub(crate) fn download_release_asset_with_identity_and_recheck(
    slug: &str,
    name: &str,
    before: (u64, u64),
    mut recheck: impl FnMut() -> Result<(u64, u64)>,
) -> Result<Vec<u8>> {
    let limit = validate_small_release_asset_size(name, before.1)?;
    // Pin the transfer to the immutable asset ID: a name-based download can race a
    // delete/re-upload and return another object's bytes. Immutability also makes the
    // retry sound: every attempt asks for the same object.
    let bytes = retry_transport_failures(
        &format!("download exact release asset {name} from {slug}"),
        ASSET_TRANSFER_BACKOFFS,
        &mut |seconds| std::thread::sleep(std::time::Duration::from_secs(seconds)),
        |_attempt| attempt_exact_release_asset_transfer(slug, before.0, before.1, limit),
    )?;
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

// The bound the client enforces, shared from aterm-update-core so the two cannot drift.
const UPDATER_MAX_DMG_BYTES: u64 = aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND;

/// The most a lean `aterm-<v>.dmg` may weigh, in decimal bytes (the units the Finder
/// and the download page show). A different question from [`UPDATER_MAX_DMG_BYTES`],
/// the client's 2 GiB download bound: "is this the lean download we promised", not
/// "can a client fetch it at all". The lean image was 40.6 MB at v0.98.0; 200 MB
/// leaves room for growth but not for a seeded container.
///
/// No environment opt-out: an exported variable would disable the check for a real
/// cut too.
const LEAN_DMG_CEILING_BYTES: u64 = 200 * 1000 * 1000;

/// The size half of a lean-ceiling refusal, `None` up to and including the
/// ceiling. Each checkpoint adds its own next step: what went wrong differs
/// between a fresh build, a mutated `dist/` and a release already on the channel.
fn lean_dmg_overweight(size: u64) -> Option<String> {
    (size > LEAN_DMG_CEILING_BYTES).then(|| {
        format!(
            "{:.2} GB ({size} bytes), over the {} MB lean ceiling",
            size as f64 / 1e9,
            LEAN_DMG_CEILING_BYTES / 1_000_000,
        )
    })
}

/// Refuse a just-packaged macOS image that is not the lean download. Pure, so the
/// boundary is a unit test: `Ok(())` up to and including the ceiling.
pub fn validate_lean_dmg_size(version: &str, size: u64) -> Result<()> {
    let Some(overweight) = lean_dmg_overweight(size) else {
        return Ok(());
    };
    Err(Error::new(format!(
        "aterm-{version}.dmg is {overweight}.\n\
         fix:  `{CUT_COMMAND} --abandon v{version}`, find what the bundle gained, cut again\n\
         why:  v0.63.0 shipped exactly this and nothing at cut time objected; the client's \
         {UPDATER_MAX_DMG_BYTES}-byte bound is about downloadability, not leanness"
    )))
}

/// Apply the lean-image contract to the bytes on disk. Every resumable suffix
/// distrusts the packager's earlier size (`dist/` is mutable, and a published recovery
/// downloads a fresh object), so metadata is read at each publication boundary,
/// without hashing a possibly gigabyte image first. `fix` is the checkpoint's own next
/// step, when it has one.
fn validate_lean_dmg_on_disk(path: &Path, checkpoint: &str, fix: Option<&str>) -> Result<()> {
    let metadata = fs::metadata(path).map_err(|error| {
        Error::new(format!(
            "{checkpoint}: read on-disk DMG metadata {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(Error::new(format!(
            "{checkpoint}: on-disk DMG is not a regular file: {}",
            path.display()
        )));
    }
    let Some(overweight) = lean_dmg_overweight(metadata.len()) else {
        return Ok(());
    };
    let fix = fix.map(|fix| format!("\nfix:  {fix}")).unwrap_or_default();
    Err(Error::new(format!(
        "{checkpoint}: {} is {overweight}.{fix}",
        path.display()
    )))
}

/// Retain a digest-verified published DMG, then judge the retained file rather
/// than trusting release metadata or the downloader's prior size observation.
/// The closure seam keeps the crash-recovery boundary directly testable
/// without a GitHub release fixture.
fn recover_lean_dmg_to<F>(destination: &Path, download: F) -> Result<VerifiedReleaseAsset>
where
    F: FnOnce(&Path) -> Result<VerifiedReleaseAsset>,
{
    let asset = download(destination)?;
    // The release is already public, and `--abandon` refuses a published release.
    validate_lean_dmg_on_disk(destination, "published recovery", None)?;
    Ok(asset)
}
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
    let sha256 = aterm_codec::hex::encode_lower(&digest.finalize());
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

/// Refuse a manifest that still names the retired Intel DMG pair
/// (`dmg_x86_64` / `dmg_x86_64_sha256`).
///
/// The wire keys stay in the shared `Manifest` so clients keep parsing published
/// manifests; this cutter never emits them, and every gate that judges a manifest
/// runs this first so the retired contract is refused by name.
pub fn refuse_retired_intel_dmg(manifest: &Manifest) -> Result<()> {
    if manifest.dmg_x86_64.is_some() || manifest.dmg_x86_64_sha256.is_some() {
        return Err(Error::new(format!(
            "manifest names the retired Intel DMG ({:?}); aterm ships one macOS DMG. \
             Finish or retire this cut with the cutter version that started it",
            manifest
                .dmg_x86_64
                .as_deref()
                .unwrap_or("<digest without a name>")
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
            // with the exact-ID bytes just verified above. `publish` re-reads this
            // path.
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

/// The anchor the shipped binary must prove it compiled in: the paper master.
/// `aterm-gui/build.rs` embeds `__DATA,__aterm_upin` from
/// `pins::PAPER_MASTER_PUBKEYS[0]`, a property of the source tree, never of the machine
/// that built it, so every rostered machine expects the same string. `None` for a fork
/// with no master (the binary embeds the zero sentinel).
pub fn expected_embedded_update_pin(master_pubkeys: &[&str]) -> Result<Option<String>> {
    master_pubkeys
        .first()
        .map(|master| update_key_fingerprint(master))
        .transpose()
}

/// Unix seconds for the roster's freshness window, failing closed the opposite way
/// from [`unix_now`]: 0 would read as 1970 and pass a lapsed roster, so an unreadable
/// clock reads as the far future and expires every window, as
/// `aterm_update_core::roster::now_unix` does.
fn roster_now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(i64::MAX, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The complete pre-claim signing verdict: whether this cut signs, with which key, as
/// which machine, under which roster document. One struct because the parts are only
/// correct together: an attribution without the roster bytes could not be published,
/// and roster bytes without an attribution authorize nothing. Every field is public
/// identity, so `Debug` is safe.
#[derive(Debug)]
pub struct SigningVerdict {
    /// Whether to sign, and with which key.
    pub policy: SignaturePolicy,
    /// Which machine the roster says this key is, when the tier is armed. `None` with
    /// an unpinned master (a fork).
    pub attribution: Option<roster::Attribution>,
    /// The exact roster bytes that authorized the cut, to be published as assets so
    /// clients can check the same document. `None` exactly when `attribution` is.
    pub roster: Option<machines::RosterDocument>,
}

impl SigningVerdict {
    /// The journaled half of the attribution: public identity only, exactly as
    /// `signature_pubkey` is.
    fn machine_id(&self) -> Option<String> {
        self.attribution.as_ref().map(|who| who.machine_id.clone())
    }

    /// The three attribution-shaped fields a [`CutCtx`] carries, derived together so
    /// an edit cannot set two of the three.
    fn cut_attribution(self) -> CutAttribution {
        CutAttribution {
            machine_id: self.machine_id(),
            attribution: self.attribution,
            roster: self.roster,
        }
    }
}

/// The attribution half of a [`CutCtx`], as one value. See
/// [`SigningVerdict::cut_attribution`].
struct CutAttribution {
    machine_id: Option<String>,
    attribution: Option<roster::Attribution>,
    roster: Option<machines::RosterDocument>,
}

impl CutAttribution {
    /// Nothing to stamp and nothing to stage — a fork's cut, and every resume past
    /// `sign`, where the manifest and roster assets are already on disk.
    const fn none() -> Self {
        Self {
            machine_id: None,
            attribution: None,
            roster: None,
        }
    }
}

/// May a resume that re-authorized as `observed` continue a cut the journal says was
/// started by `journaled`? Only as the same machine, "both nameless" (a fork)
/// included. Both asymmetric cases refuse:
///
/// * journaled `Some`, observed `None`: the cut was authorized by a roster and this
///   resume has none, so nothing proves the attribution it would sign.
/// * journaled `None`, observed `Some`: the anchor was armed mid-cut, and the bytes
///   already signed carry no attribution, so the release's halves would disagree.
pub fn resume_attribution_agrees(journaled: Option<&str>, observed: Option<&str>) -> Result<()> {
    if journaled == observed {
        return Ok(());
    }
    Err(Error::new(format!(
        "this cut was started by machine {journaled:?} but the machine roster authorizes \
         this one as {observed:?}; refusing to rebuild another machine's cut — its \
         manifest is already signed over the first machine's attribution"
    )))
}

/// The pipeline's entry into [`signing_verdict`] with the real anchors, named here and
/// nowhere below (as with [`resolve_apple_tier`]) so a test can drive the armed path
/// with a synthetic master.
fn preflight_signature_policy(
    creds: Option<&sign::ReleaseCredentials>,
    duty: RosterDuty,
) -> Result<SigningVerdict> {
    signing_verdict(
        creds,
        &SigningAnchors {
            master_pubkeys: aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
            identity_path: machines::conventional_identity_path().as_deref(),
            now_unix: roster_now_unix(),
            duty,
        },
    )
}

/// Which [`RosterDuty`] a re-entry carries: has `sign` already run? `sign` is the only
/// step that assembles, attributes and signs a manifest (`stage_manifest` →
/// `sign_manifest_with_policy`) and stages the roster assets; every later step moves
/// bytes that exist. `resume_cut`, `run_recover_lost` and
/// `revalidate_ctx_signature_policy` must agree on it.
const fn roster_duty(build_done: bool) -> RosterDuty {
    if build_done {
        RosterDuty::Finish
    } else {
        RosterDuty::Sign
    }
}

/// The anchors and ambient inputs [`signing_verdict`] resolves against — parameters,
/// never reads, so every one of them can be a synthetic value in a test.
pub struct SigningAnchors<'a> {
    /// `pins::PAPER_MASTER_PUBKEYS` in production. Empty ⇒ the tier is absent.
    pub master_pubkeys: &'a [&'a str],
    /// `~/.aterm/machine.toml` in production; `None` on a machine with no `HOME`.
    /// Consulted only when the profile declares no `machine_id`, and only ever as a
    /// cross-check.
    pub identity_path: Option<&'a Path>,
    /// Injected wall clock for the roster's freshness window.
    pub now_unix: i64,
    /// Whether this entry can still sign; see [`RosterDuty`].
    pub duty: RosterDuty,
}

pub fn signing_verdict(
    creds: Option<&sign::ReleaseCredentials>,
    anchors: &SigningAnchors<'_>,
) -> Result<SigningVerdict> {
    // The pipeline's one answer to "may this machine sign?". With the paper master
    // pinned, signing is committed channel policy: a keyless machine refuses pre-claim,
    // and a key the roster does not authorize refuses by name. Recovery and the yank
    // successor cut come through here too, so no flavor can reopen a rostered channel
    // to unsigned bytes.
    //
    // The roster is read only when armed with a `Sign` duty: an unarmed anchor costs
    // nothing, and a `Finish` duty would read a roster other than the one frozen in
    // `dist/`.
    let armed = !anchors.master_pubkeys.is_empty() && anchors.duty == RosterDuty::Sign;
    let document = match (
        armed,
        creds.and_then(sign::ReleaseCredentials::machine_roster),
    ) {
        (true, Some(path)) => Some(machines::RosterDocument::read(path)?),
        _ => None,
    };
    let declared = if armed {
        machines::declared_machine_id(
            creds.and_then(sign::ReleaseCredentials::machine_id),
            anchors.identity_path,
        )?
    } else {
        None
    };
    let (policy, attribution) = channel_signature_policy(
        load_signing_material(creds)?
            .as_ref()
            .map(|material| material.pubkey.as_str()),
        &RosterEvidence {
            master_pubkeys: anchors.master_pubkeys,
            roster: document.as_ref(),
            declared_machine_id: declared.as_deref(),
            now_unix: anchors.now_unix,
            duty: anchors.duty,
        },
    )?;
    Ok(SigningVerdict {
        // Keep the roster only when it authorized something, so roster bytes and an
        // attribution can never disagree.
        roster: attribution.as_ref().and(document),
        attribution,
        policy,
    })
}

fn sign_manifest_with_policy(ctx: &CutCtx, manifest: &Path) -> Result<PathBuf> {
    let expected_pubkey = ctx
        .signature_pubkey
        .as_deref()
        .ok_or_else(|| Error::new("signature-required cut has no persisted channel public key"))?;
    let creds = ctx.credentials.as_ref().ok_or_else(|| {
        Error::new(
            "signature-required cut has no credentials; pass --release-credentials <path> \
             (it is required on resume and recovery too, not only on the fresh cut)",
        )
    })?;
    let material = load_signing_material(Some(creds))?.ok_or_else(|| {
        Error::new("signature-required resume needs the recovered offline signing configuration")
    })?;
    if material.pubkey != expected_pubkey {
        return Err(Error::new(
            "current signing key identity differs from the journaled channel public key; \
             refusing key substitution",
        ));
    }
    // Signed in-process: no key path in a process listing, no second binary.
    let signature = manifest.with_extension("toml.sig");
    let manifest_bytes = fs::read(manifest)
        .map_err(|error| Error::new(format!("read {}: {error}", manifest.display())))?;
    let signature_bytes = creds.sign(&manifest_bytes).map_err(Error::new)?;
    fs::write(&signature, &signature_bytes)
        .map_err(|error| Error::new(format!("write {}: {error}", signature.display())))?;
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

/// Assemble the manifest, stamp the attribution into it, prove the bytes and write
/// them, in that order: `machine_id` and `roster_seq` are worth nothing outside what
/// the signature covers, and `sign_manifest_with_policy` signs the file this writes. A
/// named function (not inline in `step_sign`, which needs a real verified bundle) so
/// the ordering has a covering test.
pub fn stage_manifest(
    dist: &Path,
    inputs: &manifest_out::ManifestInputs<'_>,
    who: Option<&roster::Attribution>,
    linux: Option<&buildplan::linux::Handoff>,
) -> Result<PathBuf> {
    let mut manifest = manifest_out::build(inputs);
    if let Some(linux) = linux {
        linux.verify_staged(dist, inputs.version, inputs.build_number, inputs.commit)?;
        linux.stamp(&mut manifest)?;
    }
    // With an unpinned paper master `who` is `None` and both keys stay absent.
    if let Some(who) = who {
        machines::attribute(&mut manifest, who);
    }
    manifest_out::write(dist, &manifest)
}

/// Stage the master-signed roster beside the appcast, so a client can fetch the
/// document that authorizes the signature it checks. These are the exact bytes the
/// pre-claim gate verified, carried through the cut rather than re-read.
///
/// `None` (a fork's cut) writes and removes nothing. No stale-roster cleanup is
/// needed: `publish` uploads only `CutCtx::channel_asset_names`, and
/// `channel::validate_channel_asset_set` refuses any asset outside that set.
pub fn stage_roster_assets(dist: &Path, document: Option<&machines::RosterDocument>) -> Result<()> {
    let Some(document) = document else {
        return Ok(());
    };
    let roster = dist.join(roster::ROSTER_ASSET);
    let signature = dist.join(roster::ROSTER_SIG_ASSET);
    fs::write(&roster, &document.bytes)
        .map_err(|e| Error::new(format!("stage {}: {e}", roster.display())))?;
    fs::write(&signature, &document.signature)
        .map_err(|e| Error::new(format!("stage {}: {e}", signature.display())))?;
    step(
        "roster",
        &format!(
            "{} + {} staged as release assets ({} + {} bytes)",
            roster::ROSTER_ASSET,
            roster::ROSTER_SIG_ASSET,
            document.bytes.len(),
            document.signature.len()
        ),
    );
    Ok(())
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&Sha256::digest(bytes))
}

/// Immutable identity expected from the release under the journal's tag.
#[derive(Debug, Clone, Copy)]
pub struct ExpectedReleaseIdentity<'a> {
    pub version: &'a str,
    pub build: u64,
    pub commit: &'a str,
}

/// Validate a published release's exact appcast pair before a lost machine's
/// recovery reconstructs a journal from it. A pure seam, shared with its
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
    let expected_dmg = channel::dmg_asset_name(expected.version);
    if manifest.dmg != expected_dmg {
        return Err(Error::new(format!(
            "published manifest names DMG {:?}, expected exact {expected_dmg:?}",
            manifest.dmg
        )));
    }
    // The zip stays optional on the wire (a release cut before zip staging has
    // none), but a manifest that names one must name the canonical one: the
    // client derives this same string from the tag and refuses anything else.
    let expected_zip = channel::zip_asset_name(expected.version);
    if let Some(zip) = manifest.zip.as_deref()
        && zip != expected_zip
    {
        return Err(Error::new(format!(
            "published manifest names zip {zip:?}, expected exact {expected_zip:?}"
        )));
    }
    refuse_retired_intel_dmg(&manifest)?;
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
    /// The signing material, loaded once from `--release-credentials` and carried for
    /// the life of the cut. Never serialized: the journal keeps only `signature_pubkey`,
    /// the public identity, which is what is matched (a path proves nothing about the
    /// bytes it holds at signing time).
    pub credentials: Option<sign::ReleaseCredentials>,
    /// Tier APPLE, resolved once at the entry point, before the ledger claim, so a
    /// machine with no Developer-ID certificate or notarytool credential fails before
    /// it burns a single-use build number. `AppleTier::Inactive` whenever
    /// `pins::APPLE_TEAM_ID` is empty (a fork).
    pub apple: sign::AppleTier,
    /// The operator's checkout, holding `dist/` and the journal. The cut never builds
    /// from it and never moves it.
    pub repo: PathBuf,
    /// The tree the cut reads and builds: the cut tree ([`gates::cut_tree_path`]) at
    /// the release commit for a real cut, `repo` itself for a dry run.
    pub tree: PathBuf,
    pub dist: PathBuf,
    pub journal_path: PathBuf,
    /// THE release channel this cut publishes onto ("owner/repo"): the tracked
    /// `[workspace.metadata.aterm] update_channel` (`[workspace.package] repository`
    /// when it is absent). The ledger, the lease, the fence and the tag live on `origin`; the one release
    /// object lives here.
    pub slug: String,
    pub version: String,
    pub tag: String,
    pub build: u64,
    /// The commit artifacts must come from: for a real cut the release commit —
    /// the published commit plus the claim's ledger line and changelog roll
    /// ([`ledger::claim`]); HEAD for a dry run.
    pub commit: String,
    /// Effective carried channel floor, already validated against `build`.
    pub min_build: Option<u64>,
    /// The release cache's key of the bundle `sign` copies and signs: the one the
    /// release built once and `verify release` judged ([`Journal::artifact`]). `None`
    /// only where nothing is left to sign (a recovered, already-published cut).
    pub artifact: Option<String>,
    /// That bundle's executable SHA-256, as measured ([`Journal::macho_sha256`]).
    pub macho_sha256: Option<String>,
    pub linux: Option<buildplan::linux::Handoff>,
    /// Set by `sign` and journaled; a resume restores it.
    pub manifest_signed: bool,
    /// Frozen pre-claim channel-signature ratchet and its actual public key.
    pub signature_required: bool,
    pub signature_pubkey: Option<String>,
    /// The key the published artifacts must verify under ([`Journal::verify_pubkey`]).
    /// `None` means [`Self::signature_pubkey`]: every cut except a cross-machine
    /// recovery of someone else's release.
    pub verify_pubkey: Option<String>,
    /// The machine the roster authorized, journaled beside the public key. Restored on
    /// every resume, including past `sign`, where [`CutCtx::attribution`] is not: the id
    /// is what every later step must keep agreeing with; the full attribution is needed
    /// only to stamp a manifest.
    pub signature_machine_id: Option<String>,
    /// The full attribution (id + key + `roster_seq`) for the one step that stamps it
    /// into the manifest, and the roster bytes that authorized it, to be published as
    /// assets. `Some` only when `sign` will still run under an armed anchor, the rule
    /// [`resume_apple_tier`] applies to the Developer-ID certificate.
    pub attribution: Option<roster::Attribution>,
    pub roster: Option<machines::RosterDocument>,
    /// The channel release's immutable GitHub ID, persisted in the real-cut journal
    /// the moment it is bound ([`Journal::release_id`]).
    pub release_id: Option<u64>,
    /// [`Journal::release_intent`].
    pub release_intent: bool,
    /// [`Journal::upload_intents`].
    pub upload_intents: Vec<String>,
    pub kind: CutKind,
    /// Present only for a real cut while its remote owner ref is held.
    pub lease: Option<ReleaseLeaseGuard>,
    /// Unique per-invocation token; two same-claim resumes cannot share it.
    pub fence: Option<PublisherFenceGuard>,
    /// Which changelog section carries this cut's notes: the rolled
    /// `[version]` for a real cut, `[Unreleased]` for a dry run (no roll ever
    /// happens there).
    pub notes_section: String,
    /// Some(..) for a real cut; a dry run is deliberately unjournaled
    /// (a provisional n must never look resumable).
    pub journal: Option<Journal>,
}

impl CutCtx {
    fn linux_asset_names(&self) -> Vec<String> {
        self.linux.as_ref().map_or_else(Vec::new, |handoff| {
            handoff
                .artifacts
                .iter()
                .map(|artifact| artifact.asset.clone())
                .collect()
        })
    }

    /// Exactly the asset set this cut's channel release carries (beside the engine's
    /// source attestation pair), sorted: the client set and the native Linux
    /// executables, never the debugging aids, which stay in `dist/` (see `channel`'s
    /// module note). The upload set and the acceptance rule both read it, so they cannot
    /// drift apart.
    fn channel_asset_names(&self) -> Vec<String> {
        let mut names = channel::required_asset_names(
            &self.version,
            self.signature_required,
            self.attaches_roster(),
        );
        names.extend(self.linux_asset_names());
        names.sort();
        names
    }

    /// [`channel::validate_channel_asset_set`] of `names` against this cut's set.
    fn validate_channel_assets(&self, names: &[String]) -> Result<()> {
        channel::validate_channel_asset_set(
            names,
            &self.version,
            self.signature_required,
            self.attaches_roster(),
            &self.linux_asset_names(),
        )
    }

    fn dmg_path(&self) -> PathBuf {
        self.dist.join(channel::dmg_asset_name(&self.version))
    }
    /// The updater container (`ditto` zip). Same bundle as the DMG, staged
    /// without `hdiutil` — see `dmg::create_zip`.
    fn zip_path(&self) -> PathBuf {
        self.dist.join(channel::zip_asset_name(&self.version))
    }
    /// What published bytes must verify under. Falls back to this machine's own
    /// signing key, which is the same value on every cut that is not recovering
    /// another machine's release.
    fn verification_pubkey(&self) -> Option<&str> {
        self.verify_pubkey
            .as_deref()
            .or(self.signature_pubkey.as_deref())
    }

    /// This cut's bundle, `dist/cut-<build>.noindex/aterm.app`
    /// ([`bundle::staged_app_path`]).
    fn app_path(&self) -> PathBuf {
        bundle::staged_app_path(&self.dist, self.build)
    }
    /// The DMG's `.sha256` sidecar in dist/ — written by `step_sign` from the
    /// in-process digest, verified against the manifest by `step_selfcheck`.
    fn dmg_sha256_path(&self) -> PathBuf {
        self.dist
            .join(channel::sha256_sidecar_name(&channel::dmg_asset_name(
                &self.version,
            )))
    }
    fn zip_sha256_path(&self) -> PathBuf {
        self.dist
            .join(channel::sha256_sidecar_name(&channel::zip_asset_name(
                &self.version,
            )))
    }
    /// The stable download twins in dist/ (`aterm.dmg` / `aterm-mac.zip`), byte copies
    /// of the canonical containers: staged by `step_sign` from the final packaged bytes,
    /// re-proved (or, when missing, regenerated) by `step_selfcheck`, published through
    /// `channel_asset_paths`.
    fn stable_dmg_path(&self) -> PathBuf {
        self.dist.join(channel::stable_dmg_asset_name())
    }
    fn stable_zip_path(&self) -> PathBuf {
        self.dist.join(channel::stable_zip_asset_name())
    }
    /// The twins' `.sha256` sidecars — the same digests the versioned sidecars
    /// state, with the alias filename embedded, because `shasum -a 256 -c`
    /// matches on the embedded name and a `releases/latest/download/...` click
    /// saves the alias name.
    fn stable_dmg_sha256_path(&self) -> PathBuf {
        self.dist.join(channel::sha256_sidecar_name(
            &channel::stable_dmg_asset_name(),
        ))
    }
    fn stable_zip_sha256_path(&self) -> PathBuf {
        self.dist.join(channel::sha256_sidecar_name(
            &channel::stable_zip_asset_name(),
        ))
    }
    fn manifest_path(&self) -> PathBuf {
        self.dist.join(manifest_out::MANIFEST_ASSET)
    }
    fn notes_path(&self) -> PathBuf {
        self.dist.join(format!("notes-{}.md", self.version))
    }
    fn provenance_path(&self) -> PathBuf {
        self.dist
            .join(channel::provenance_asset_name(&self.version))
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
                "GitHub release ID is zero or differs from the already-bound release capability",
            ));
        }
        self.release_id = Some(id);
        if let Some(journal) = &mut self.journal {
            if journal.release_id.is_some_and(|current| current != id) {
                return Err(Error::new(
                    "journaled GitHub release ID differs from the observed release capability",
                ));
            }
            journal.release_id = Some(id);
            journal.save(&self.journal_path)?;
        }
        Ok(())
    }

    /// Persist the durable release intent — before the one create POST, or before a
    /// visible release under this tag is adopted — and mint the one permit that POST
    /// consumes. An adoption leaves the permit unused: nothing will POST, the visible
    /// release is the object the intent covers.
    pub(crate) fn persist_release_intent(&mut self) -> Result<DurablePostPermit> {
        if self.release_intent {
            return Err(Error::new(
                "release intent already exists; refusing to mint another process-local POST permit",
            ));
        }
        if self.kind == CutKind::Real && self.journal.is_none() {
            return Err(Error::new(
                "real release bind has no durable journal; refusing to mint a POST permit",
            ));
        }
        self.release_intent = true;
        if let Some(journal) = &mut self.journal {
            journal.release_intent = true;
            journal.save(&self.journal_path)?;
        }
        Ok(DurablePostPermit(()))
    }

    /// Undo [`Self::persist_release_intent`] for the one outcome that proves the create
    /// POST created nothing: a 4xx answer ([`server_refused_without_creating`]). Never
    /// a general "clear the flag": every other failure can hide a delivered request.
    pub(crate) fn release_release_intent(&mut self) -> Result<()> {
        self.release_intent = false;
        if let Some(journal) = &mut self.journal {
            journal.release_intent = false;
            journal.save(&self.journal_path)?;
        }
        Ok(())
    }

    fn upload_intent_issued(&self, name: &str) -> bool {
        self.upload_intents.iter().any(|issued| issued == name)
    }

    pub(crate) fn persist_upload_intent(&mut self, name: &str) -> Result<DurablePostPermit> {
        self.record_upload_intent(name)?;
        Ok(DurablePostPermit(()))
    }

    /// Journal `name` as an asset this cut owns on the release, without minting a POST
    /// permit ([`Self::persist_upload_intent`] is this plus the permit, and the only
    /// door to a POST). This serves an object already there: `--abandon` withdraws
    /// exactly the journaled upload intents ([`channel::withdrawable_assets`]), so an app
    /// asset no intent names could not be taken back, and a recut would refuse it.
    fn record_upload_intent(&mut self, name: &str) -> Result<()> {
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
        Ok(())
    }

    /// Undo an upload intent for a POST that provably created nothing. Only
    /// [`Self::settle_upload_intent`] leads here: [`transport_never_started`] (curl exit
    /// 2, before connect) or [`server_refused_without_creating`] (a 4xx). Keeping the
    /// intent would wedge every resume on a POST that never happened.
    fn retract_upload_intent(&mut self, name: &str) -> Result<()> {
        self.upload_intents.retain(|issued| issued != name);
        if let Some(journal) = &mut self.journal {
            journal.upload_intents.retain(|issued| issued != name);
            journal.save(&self.journal_path)?;
        }
        Ok(())
    }

    /// Settle the durable intent of an upload POST that has just finished: retract it
    /// exactly when the POST provably created nothing ([`post_outcome`]), else keep it;
    /// the one place an upload intent is spent. A 4xx counts as for the release create
    /// (GitHub holds one asset per name, so repeating a stored object is refused the
    /// same way), but re-proves the lease first ([`ensure_ctx_release_lease`]): a
    /// process fenced out during a long POST must not overwrite its successor's journal.
    /// Curl's exit 2 skips that proof: under memory pressure one more fallible call
    /// before the retraction would restore the permanent wedge.
    pub(crate) fn settle_upload_intent(&mut self, name: &str, out: &RunOut) -> Result<PostOutcome> {
        let outcome = post_outcome(out);
        match outcome {
            PostOutcome::Unknown => {}
            PostOutcome::NeverSent => self.retract_upload_intent(name)?,
            PostOutcome::Refused => {
                ensure_ctx_release_lease(self).map_err(|error| {
                    Error::new(format!(
                        "upload of {name} was answered with a 4xx, which created nothing, but \
                         this process could not re-prove it still owns the release ({error}); \
                         the durable intent was KEPT rather than rewrite a journal a successor \
                         may own. If this process is still the cut's owner, `--abandon` \
                         withdraws the cut and a recut starts clean"
                    ))
                })?;
                self.retract_upload_intent(name)?;
            }
        }
        Ok(outcome)
    }

    /// Local paths of exactly the assets this cut publishes, in a stable order.
    fn channel_asset_paths(&self) -> Vec<PathBuf> {
        self.channel_asset_names()
            .into_iter()
            .map(|name| self.dist.join(name))
            .collect()
    }

    /// Does this cut publish the machine roster? Exactly when it has an attributed
    /// machine (the paper master is pinned), read from the journaled id so the answer
    /// survives a resume past `sign`, which has no roster document in hand.
    fn attaches_roster(&self) -> bool {
        self.signature_machine_id.is_some()
    }

    /// Which fingerprint the shipped binary must prove it compiled in: always the
    /// committed paper master, never the machine that signs. One accessor so
    /// `step_sign` (which writes it into the provenance) and `step_selfcheck` (which
    /// checks the binary and the provenance against it) cannot derive it differently.
    fn expected_embedded_pin(&self) -> Result<Option<String>> {
        expected_embedded_update_pin(aterm_update_core::pins::PAPER_MASTER_PUBKEYS)
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

/// What a cut that never made its release the head knows about the release it
/// touched: its journal's release intent, bound ID and upload intents — or nothing at
/// all, for a lost machine's recovery.
#[derive(Debug, Clone, Copy)]
pub struct WithdrawKnowledge<'a> {
    pub release_intent: Option<bool>,
    pub release_id: Option<u64>,
    pub upload_intents: Option<&'a [String]>,
    /// [`RECOVERY_NO_DRAFT_POSTED_FLAG`]: the operator answers for a lost journal that
    /// no create POST ever landed.
    pub operator_asserts_no_post: bool,
}

impl<'a> WithdrawKnowledge<'a> {
    /// Everything `journal` records.
    #[must_use]
    pub fn of(journal: &'a Journal) -> Self {
        Self {
            release_intent: Some(journal.release_intent),
            release_id: journal.release_id,
            upload_intents: Some(&journal.upload_intents),
            operator_asserts_no_post: false,
        }
    }
}

/// Withdraw what an unpublished cut put on the channel, and nothing else: the remote
/// half of `--abandon` and of a lost machine's recovery. A draft this cut created is
/// deleted by ID ([`draft_cleanup_decision`]); the engine's source release this cut
/// adopted loses exactly this cut's uploads ([`channel::withdrawable_assets`]: the
/// journal's upload intents, or for a lost journal this version's assets, a foreign
/// name refusing) and is never deleted; an absent release converges only when no
/// create POST can still become visible ([`absent_draft_decision`]). A published app
/// release is `yank`'s. Every DELETE re-proves the owner and process token first.
/// Returns the transcript line.
pub fn withdraw_unpublished_release(
    repo: &Path,
    slug: &str,
    tag: &str,
    version: &str,
    known: WithdrawKnowledge<'_>,
    lease: &ReleaseLeaseGuard,
    fence: &PublisherFenceGuard,
) -> Result<String> {
    let git = GitCli::new(repo);
    assert_publisher_session(&git, lease, fence)?;
    let state = verify::release_state(slug, tag)?;
    if state == verify::ReleaseState::Published {
        return Err(Error::new(format!(
            "{tag} is the channel's PUBLISHED app release on {slug} — withdrawing it is not \
             an abandon; retire a published build with `{SHIP_COMMAND} yank <build> \
             --release-credentials <profile.toml> --mac-only|--linux-artifacts DIRECTORY`"
        )));
    }
    let Some(release) = unique_release_object_by_tag(slug, tag)? else {
        if absent_draft_decision(known.release_intent, known.operator_asserts_no_post)
            == AbsentDraftDecision::RetainOwnerAwaitVisibility
        {
            let (why, remedy) = if known.release_intent == Some(true) {
                (
                    "known issued",
                    "wait for the exact release to converge and run the command again",
                )
            } else {
                (
                    "unknown because the current journal is unavailable",
                    "if the channel's releases page shows NO release for this tag, re-run with \
                     --no-draft-was-posted to release the claim lease",
                )
            };
            return Err(Error::new(format!(
                "release {tag} is currently absent from {slug}, but its create intent is \
                 {why}. An accepted POST may still become visible; retaining the claim lease \
                 and refusing tag/journal cleanup until it converges — {remedy}"
            )));
        }
        return Ok(format!("no release {tag} on {slug} — nothing to withdraw"));
    };
    if let Some(expected) = known.release_id
        && expected != release.id
    {
        return Err(Error::new(format!(
            "{tag} on {slug} resolves to release ID {}, not the ID {expected} this cut bound; \
             refusing to touch a replacement",
            release.id
        )));
    }
    let inventory = release_asset_inventory_for_release_id(slug, release.id)?;
    let names: Vec<String> = inventory.iter().map(|asset| asset.name.clone()).collect();
    if release.draft {
        match draft_cleanup_decision(
            known.release_intent,
            true,
            channel::binds_to_version(&names, version),
        ) {
            DraftCleanupDecision::DeleteIssuedVisible => {}
            DraftCleanupDecision::AbandonProvenNoPost
            | DraftCleanupDecision::RetainIssuedAwaitVisibility
            | DraftCleanupDecision::RefuseUnknownOrInconsistent => {
                return Err(Error::new(format!(
                    "the draft {tag} on {slug} (ID {}) is not provably this cut's: this \
                     journal issued no create for it, or — with no journal — it carries \
                     {names:?}, which is not only this version's assets. Retaining the claim \
                     lease; inspect it by hand",
                    release.id
                )));
            }
        }
        delete_release_object_by_id_with_guard(
            slug,
            &release,
            false,
            || Ok(()),
            || assert_publisher_session(&git, lease, fence),
        )?;
        if !release_objects_by_tag(slug, tag)?.is_empty() {
            return Err(Error::new(format!(
                "draft {tag} (ID {}) was deleted but the tag now resolves to a replacement \
                 on {slug}; refusing tag/lease cleanup",
                release.id
            )));
        }
        return Ok(format!(
            "draft {tag} (ID {}) deleted from {slug}",
            release.id
        ));
    }
    let withdraw =
        channel::withdrawable_assets(&names, version, known.upload_intents).map_err(|why| {
            Error::new(format!(
                "the source release {tag} on {slug} (ID {}) {why}; retaining the claim lease — \
                 inspect it by hand",
                release.id
            ))
        })?;
    for name in &withdraw {
        let asset = inventory
            .iter()
            .find(|asset| &asset.name == name)
            .expect("withdrawable names come from this inventory");
        let observed = release_object_by_id(slug, release.id)?;
        validate_release_object_snapshot(observed.as_ref(), &release)?;
        assert_publisher_session(&git, lease, fence)?;
        let endpoint = format!("repos/{slug}/releases/assets/{}", asset.id);
        let out = gh_raw(&["api", "--method", "DELETE", &endpoint])?;
        if release_asset_identity_for_release_id_optional(slug, release.id, name)?
            .is_some_and(|(id, _)| id == asset.id)
        {
            return Err(Error::new(format!(
                "delete {name} (asset ID {}) from {tag} on {slug} failed: {}",
                asset.id,
                out.stderr_utf8().trim()
            )));
        }
    }
    Ok(format!(
        "{} asset(s) this cut uploaded withdrawn from {tag} on {slug} (ID {}); it stays the \
         engine's source release, and a recut adopts it again",
        withdraw.len(),
        release.id
    ))
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

/// Resume requires a clean tree, with no exceptions: no step mutates the
/// checkout before committing, so there is no legitimately dirty state to admit.
pub fn recovery_resume_worktree_preflight(
    _repo: &Path,
    git: &dyn GitRunner,
    _journal: &Journal,
) -> Result<()> {
    gates::clean_tree(git)
}

/// Bind an ordinary `--resume` to the immutable claim before the pipeline can
/// reacquire either publication ref. The journal is only a crash cursor, never
/// authority for `(version, build, commit)`, so a hand-edited journal cannot steer a
/// late upload or flip. The worktree check runs first, so a dirty path is rejected
/// before even the read-only fetch.
pub fn ordinary_resume_claim_preflight(
    repo: &Path,
    git: &dyn GitRunner,
    journal: &Journal,
) -> Result<()> {
    recovery_resume_worktree_preflight(repo, git, journal)?;
    gates::current_own_cutter_gate(git)?;

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
    // No HEAD clause: nothing an abandon or a retire does reads a tree, and a resume
    // or a recovery puts the cut tree at the claim itself (`gates::place_cut_tree`).
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

/// Explicit cross-machine recovery for a persistent lease whose local journal was
/// lost. An unpublished cut is withdrawn from the channel
/// ([`withdraw_unpublished_release`]); an already-published exact-identity cut is
/// reconstructed at `publish`, which converges without re-uploading a byte, and
/// finished through unlock. A published release is never deleted here.
/// `old_process_stopped` is the operator's assertion, not a machine proof; false
/// refuses before reading any state. A recovery that finds this machine's journal
/// finishes it as a resume does: in the cut tree at the claim, as the claim's own
/// cutter ([`gates::own_cutter`]).
pub fn run_recover_lost(
    repo: &Path,
    version: &str,
    owner: &str,
    old_process_stopped: bool,
    operator_asserts_no_post: bool,
    credentials: Option<&sign::ReleaseCredentials>,
) -> Result<()> {
    if !old_process_stopped {
        return Err(Error::new(RECOVERY_STOPPED_PROCESS_REFUSAL));
    }
    ledger::check_version_shape(version)?;
    if !valid_lease_owner(owner) {
        return Err(Error::new(
            "recover needs the full 40- or 64-hex claim commit (the cut's `lock` line prints it)",
        ));
    }
    let owner = owner.to_ascii_lowercase();
    let cargo_text = fs::read_to_string(repo.join("Cargo.toml"))
        .map_err(|error| Error::new(format!("read Cargo.toml: {error}")))?;
    let origin_slug = repo_slug(&cargo_text)
        .ok_or_else(|| Error::new("Cargo.toml repository is not an exact GitHub OWNER/REPO URL"))?;
    let slug = channel_slug(&cargo_text)?;
    let git = GitCli::new(repo);
    assert_origin_repo_binding(&git, &origin_slug)?;
    // Every `gh` call a recovery makes is a release-channel call.
    let _cred = ChannelCred::enter();
    let journal_path = repo.join("dist/cut-state.toml");
    // The header first, as every entry point reads it: a local journal of any format
    // is this claim's, and the cutter built at the claim finishes it.
    let header = JournalHeader::read(&journal_path)?;
    if let Some(header) = &header
        && (header.version != version || !header.commit.eq_ignore_ascii_case(&owner))
    {
        return Err(Error::new(format!(
            "local journal is v{} owner {}, not requested recovery v{version} {owner}",
            header.version, header.commit
        )));
    }
    if release_lease_owner(&git)?.as_deref() != Some(owner.as_str()) {
        return Err(Error::new(format!(
            "persistent release lease is not owned by supplied claim {owner}; refusing recovery"
        )));
    }
    let build = recovery_claim_build(&git, version, &owner)?;
    if let Some(header) = &header
        && header.build_number != build
    {
        return Err(Error::new(format!(
            "local journal build {} differs from claim ledger tail {build}",
            header.build_number
        )));
    }
    // Recovery can rotate the old publisher fence and mutate a live release
    // without entering the fresh-cut gate ladder. Prove this binary is the one the
    // recovery owes before any such mutation: with a local journal, the claim's own
    // cutter in the cut tree at the claim (handing off to it when this is not);
    // without one, this checkout's.
    let (tree, journal) = if header.is_some() {
        git_ok(&git, &["fetch", "origin", "main"])?;
        let tree = gates::place_cut_tree(&git, repo, &owner)?.path;
        gates::own_cutter_gate(&GitCli::new(&tree), &owner, false)?;
        let journal = Journal::load(&journal_path)?.ok_or_else(|| {
            Error::new(format!(
                "{} vanished during recovery",
                journal_path.display()
            ))
        })?;
        recovery_resume_worktree_preflight(&tree, &GitCli::new(&tree), &journal)?;
        (tree, Some(journal))
    } else {
        gates::current_own_cutter_gate(&git)?;
        recovery_worktree_preflight(&git)?;
        (repo.to_path_buf(), None)
    };
    let ancestor = git.git(&["merge-base", "--is-ancestor", &owner, "origin/main"])?;
    if !ancestor.success() {
        return Err(Error::new(format!(
            "recovery claim {owner} is not an ancestor of origin/main; refusing an unbound lease"
        )));
    }

    // The release-state probe precedes the fence rotation, so an unreachable remote
    // fails recovery before its first mutation.
    verify::release_state(&slug, &format!("v{version}"))?;
    // Validate the signing identity before rotating a killed process's token, so a
    // missing key leaves the old fence untouched and the channel visibly blocked, never
    // silently unsigned. A journal short of `sign` re-signs and re-proves the roster;
    // one past it, or none (a published release being finished), has nothing to sign.
    let duty = roster_duty(journal.as_ref().is_none_or(|j| j.is_done("sign")));
    let signature_verdict = preflight_signature_policy(credentials, duty)?;
    let signature_policy = signature_verdict.policy.clone();
    if let Some(journal) = &journal
        && (journal.signature_required != signature_policy.required
            || journal.signature_pubkey.as_deref() != signature_policy.pubkey.as_deref())
    {
        return Err(Error::new(
            "recovery journal signing policy/key differs from the current signing configuration",
        ));
    }
    // Attribution is compared only when this recovery will re-sign; finishing
    // already-signed bytes chooses nothing.
    if duty == RosterDuty::Sign
        && let Some(journal) = &journal
    {
        resume_attribution_agrees(
            journal.signature_machine_id.as_deref(),
            signature_verdict.machine_id().as_deref(),
        )?;
    }

    // This is the last line before the first recovery mutation. The flag is an
    // explicit operator assertion; no local program can prove a process on a
    // lost machine is quiescent or cancel its already-issued REST request.
    step("recover", RECOVERY_STOPPED_PROCESS_BANNER);
    let fence = rotate_publisher_fence_for_recovery(&git, &owner)?;
    // This machine's own journal is finished the way a resume finishes it; only a
    // lost journal is recovered from what the channel holds.
    let result = if let Some(journal) = journal {
        confirm_release_lease_owner(&git, &owner).and_then(|lease| {
            resume_cut(
                ResumePaths {
                    repo,
                    tree: &tree,
                    dist: &repo.join("dist"),
                    journal_path: &journal_path,
                },
                &slug,
                journal,
                Instant::now(),
                Some((lease, fence.clone())),
                credentials,
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
                operator_asserts_no_post,
            },
            &fence,
            credentials,
        )
    };
    combine_with_fence_release(result, &git, &fence)
}

struct LostRecoveryPlan<'a> {
    version: &'a str,
    build: u64,
    owner: &'a str,
    /// [`RECOVERY_NO_DRAFT_POSTED_FLAG`] was given.
    operator_asserts_no_post: bool,
}

/// A lost journal's recovery, decided by what the channel holds under the tag: the
/// published app release is finished; anything else is withdrawn, and the tag, the
/// lease and the fence go.
fn recover_under_fence(
    repo: &Path,
    slug: &str,
    plan: LostRecoveryPlan<'_>,
    fence: &PublisherFenceGuard,
    credentials: Option<&sign::ReleaseCredentials>,
) -> Result<()> {
    let LostRecoveryPlan {
        version,
        build,
        owner,
        operator_asserts_no_post,
    } = plan;
    let git = GitCli::new(repo);
    let lease = confirm_release_lease_owner(&git, owner)?;
    assert_publisher_session(&git, &lease, fence)?;
    let tag = format!("v{version}");
    if verify::release_state(slug, &tag)? == verify::ReleaseState::Published {
        let fresh_policy = fresh_published_recovery_signature_policy(slug, version, credentials)?;
        return recover_published_cut(
            repo,
            slug,
            version,
            build,
            owner,
            &fresh_policy,
            lease,
            fence.clone(),
            credentials,
        );
    }
    // The explicit recover command is the operator's assertion that the killed
    // publisher is stopped; cooperative contenders are excluded by our fresh exact
    // token, re-proved immediately before each destructive operation.
    let withdrawn = withdraw_unpublished_release(
        repo,
        slug,
        &tag,
        version,
        WithdrawKnowledge {
            release_intent: None,
            release_id: None,
            upload_intents: None,
            operator_asserts_no_post,
        },
        &lease,
        fence,
    )?;
    step("recover", &withdrawn);
    withdraw_tag_then_unlock(&git, &tag, owner, &lease, fence)?;
    step("recover", "unpublished cut withdrawn · release lock freed");
    Ok(())
}

fn fresh_published_recovery_signature_policy(
    slug: &str,
    version: &str,
    credentials: Option<&sign::ReleaseCredentials>,
) -> Result<SignaturePolicy> {
    let state = verify::release_state(slug, &format!("v{version}"))?;
    if state != verify::ReleaseState::Published {
        return Err(Error::new(format!(
            "recovery release v{version} changed away from Published while refreshing its signature authority"
        )));
    }
    // Only the policy half is refreshed here. A published recovery rebuilds nothing —
    // it validates and finishes bytes that already shipped — so the attribution it
    // must record is the one inside those bytes, read from the downloaded manifest by
    // `recover_published_cut`, never a fresh local claim about who this machine is.
    Ok(preflight_signature_policy(credentials, RosterDuty::Finish)?.policy)
}

/// Which assets a published release must carry for its own manifest to make sense,
/// beyond the ones every release has: derived from the manifest's `machine_id`, not
/// local state, so a recovery reconstructs the set `publish` demands.
fn recovered_roster_asset_names(manifest: &Manifest) -> Vec<&'static str> {
    if manifest.machine_id.is_none() {
        return Vec::new();
    }
    vec![roster::ROSTER_ASSET, roster::ROSTER_SIG_ASSET]
}

/// The public key a published release's manifest signature must verify under: a
/// property of the release, not of the recovering machine, whose
/// [`RosterDuty::Finish`] policy carries its own key. On the armed path the release
/// names its signer, and the master-signed roster beside it maps that name to a key
/// (the binding a client checks, `Attribution::bind`). Revocation is not re-judged
/// ([`machines::verify_published_roster`]): revoking a machine later does not unsign
/// what it signed. A release with no `machine_id` is a fork's, and the policy's own
/// key is the authority.
fn published_manifest_signature_pubkey(
    slug: &str,
    release_id: u64,
    manifest_bytes: &[u8],
    policy_pubkey: Option<&str>,
) -> Result<Option<String>> {
    let text = std::str::from_utf8(manifest_bytes)
        .map_err(|_| Error::new("published manifest is not UTF-8"))?;
    let manifest = Manifest::parse(text)
        .map_err(|error| Error::new(format!("published manifest parse failed: {error}")))?;
    let Some(machine_id) = manifest.machine_id.as_deref() else {
        return Ok(policy_pubkey.map(str::to_string));
    };
    let roster_bytes =
        download_release_asset_for_release_id(slug, release_id, roster::ROSTER_ASSET)?;
    let roster_sig =
        download_release_asset_for_release_id(slug, release_id, roster::ROSTER_SIG_ASSET)?;
    // Proves authorship (paper master) and that this is this release's roster — the
    // manifest's signed `machine_id`/`roster_seq` must match the document.
    let _asset_generation = machines::verify_published_roster(
        aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        roster_bytes.clone(),
        &roster_sig,
        machine_id,
        manifest.roster_seq,
    )?;
    let verified = roster::verify_roster(
        aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        roster_bytes,
        &roster_sig,
    )
    .map_err(|e| Error::new(format!("published machine roster does not verify ({e:?})")))?;
    let parsed = roster::Roster::parse(&verified)
        .map_err(|e| Error::new(format!("published machine roster is unusable ({e:?})")))?;
    let machine = parsed
        .machines
        .iter()
        .find(|m| m.id == machine_id)
        .ok_or_else(|| {
            Error::new(format!(
                "the published release is attributed to machine {machine_id:?}, which its \
                 own master-signed roster does not name"
            ))
        })?;
    Ok(Some(machine.pubkey.clone()))
}

/// Refuse to reconstruct a roster older than the one this machine is already
/// authorized by. `dist/aterm-machines.toml` is the machine's authorizing roster (what
/// `atpkg-keys` writes and `ReleaseCredentials::resolve` adopts), and `dist/` is
/// git-ignored, so it is the only copy: overwriting a newer generation (a local
/// revocation not yet published) with an older release's would silently undo it.
fn refuse_roster_downgrade(dist: &Path, incoming_seq: u64) -> Result<()> {
    let local = dist.join(roster::ROSTER_ASSET);
    let (Ok(bytes), Ok(sig)) = (
        fs::read(&local),
        fs::read(dist.join(roster::ROSTER_SIG_ASSET)),
    ) else {
        // No local pair (or half of one): there is nothing here to protect.
        return Ok(());
    };
    // Only a master-signed local roster can outrank the release's. An unverifiable
    // file is not an authorizing document, and letting one block a recovery would
    // hand any stray bytes in `dist/` a veto over un-wedging the release pipeline.
    let Ok(verified) =
        roster::verify_roster(aterm_update_core::pins::PAPER_MASTER_PUBKEYS, bytes, &sig)
    else {
        return Ok(());
    };
    let Ok(existing) = roster::Roster::parse(&verified) else {
        return Ok(());
    };
    if existing.roster_seq > incoming_seq {
        return Err(Error::new(format!(
            "{} holds roster_seq {}, NEWER than the recovered release's {incoming_seq}; \
             overwriting it would destroy the only copy of that generation (and any \
             revocation in it). Publish the newer roster first, or move the pair aside \
             to go back on purpose",
            local.display(),
            existing.roster_seq
        )));
    }
    Ok(())
}

/// Rebuild `dist/`'s roster pair from the published release, for a recovery that
/// found an attributed manifest. Required: an attributed manifest makes
/// `CutCtx::attaches_roster` true, and `publish` hard-errors on any release asset that
/// is not a local file. The manifest carries no SHA-256 for these two (the master signs
/// them, not the release key), so `machines::verify_published_roster` binds them: the
/// master signature proves authorship, and `roster_seq`/`machine_id` prove it is this
/// release's roster.
fn reconstruct_roster_assets(
    slug: &str,
    release_id: u64,
    names: &[String],
    manifest: &Manifest,
    dist: &Path,
) -> Result<()> {
    let required = recovered_roster_asset_names(manifest);
    if required.is_empty() {
        return Ok(());
    }
    let machine_id = manifest
        .machine_id
        .as_deref()
        .expect("required is non-empty exactly when machine_id is Some");
    for name in &required {
        if !exact_asset_present(names, name)? {
            return Err(Error::new(format!(
                "published recovery release is attributed to machine {machine_id:?} but \
                 carries no exact {name}; an armed client refuses such a release \
                 structurally, so there is nothing here to recover"
            )));
        }
    }
    let roster_bytes =
        download_release_asset_for_release_id(slug, release_id, roster::ROSTER_ASSET)?;
    let roster_sig =
        download_release_asset_for_release_id(slug, release_id, roster::ROSTER_SIG_ASSET)?;
    // Compare the asset's generation, which is what lands in dist/: it may be newer
    // than the manifest's attribution after a join re-dressed the release.
    let incoming_seq = machines::verify_published_roster(
        aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        roster_bytes.clone(),
        &roster_sig,
        machine_id,
        manifest.roster_seq,
    )?;
    refuse_roster_downgrade(dist, incoming_seq)?;
    // Through the roster pair's writer lock and redo transaction, not two bare writes:
    // a death between them would leave a torn pair (new document, old signature) that
    // no client verifies. `verify_published_roster` proved the bytes above.
    crate::provision::publish_proven_pair(
        &dist.join(roster::ROSTER_ASSET),
        &roster_bytes,
        &roster_sig,
    )
    .map_err(|error| Error::new(format!("reconstruct machine roster: {error}")))?;
    step(
        "recover",
        &format!(
            "reconstructed {} + {} and proved them under the pinned paper master \
             (machine {machine_id}, roster generation {incoming_seq}; the manifest is \
             attributed under {:?})",
            roster::ROSTER_ASSET,
            roster::ROSTER_SIG_ASSET,
            manifest.roster_seq
        ),
    );
    Ok(())
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
    credentials: Option<&sign::ReleaseCredentials>,
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
    validate_channel_release_capability(Some(&release_object), release_object.id, &tag, false)?;
    let (manifest_bytes, signature_bytes) =
        download_live_manifest_pair(slug, release_object.id, &tag)?;
    // The key comes from the release, never from this machine.
    let recovered_pubkey = published_manifest_signature_pubkey(
        slug,
        release_object.id,
        &manifest_bytes,
        signature_policy.pubkey.as_deref(),
    )?;
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
        recovered_pubkey.as_deref(),
    )?;
    if !buildplan::linux::manifest_asset_names(&manifest)?.is_empty() {
        return Err(Error::new(
            "published Linux recovery requires the original cut journal and frozen native handoff; do not reconstruct worker provenance from remote filenames",
        ));
    }
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
    // The updater container must be recoverable too: `publish` re-proves the channel
    // release against the reconstructed dist/, and the asset set includes the zip.
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
    // The claim identity is the signed manifest's: `validate_live_release_identity`
    // above bound its version, build and full commit to this claim.

    // Reconstruct only authoritative, remotely validated bytes. The journal resumes
    // at `publish`: nothing is rebuilt or re-uploaded from guesses, and `publish` finds
    // the head made (`ChannelRelease::head_made`), re-sends only its PATCH and proves the
    // read side against these bytes.
    let dist = repo.join("dist");
    fs::create_dir_all(&dist)
        .map_err(|error| Error::new(format!("create {}: {error}", dist.display())))?;
    let recovered_dmg = dist.join(&manifest.dmg);
    recover_lean_dmg_to(&recovered_dmg, |destination| {
        verify_release_asset_digest_for_release_id_to(
            slug,
            release_object.id,
            &tag,
            &manifest.dmg,
            &manifest.sha256,
            destination,
        )
    })?;
    // Regenerate the stable download twins from the digest-verified canonical
    // containers: recovery must leave dist/ able to satisfy the channel asset set,
    // and each twin is a byte copy of its canonical asset (`aterm.dmg` of
    // manifest.dmg).
    fs::copy(&recovered_dmg, dist.join(channel::stable_dmg_asset_name()))
        .map_err(|error| Error::new(format!("reconstruct stable dmg twin: {error}")))?;
    verify_release_asset_digest_for_release_id_to(
        slug,
        release_object.id,
        &tag,
        &recovered_zip,
        &recovered_zip_sha256,
        &dist.join(&recovered_zip),
    )?;
    fs::copy(
        dist.join(&recovered_zip),
        dist.join(channel::stable_zip_asset_name()),
    )
    .map_err(|error| Error::new(format!("reconstruct stable zip twin: {error}")))?;
    // A manifest that still names the retired Intel DMG pair was cut under a
    // container contract this cutter does not publish: refuse the takeover rather
    // than reconstruct a dist/ the exact-set gate would refuse anyway.
    refuse_retired_intel_dmg(&manifest)?;
    // The `.sha256` sidecars are pure functions of the digests just proved, so recovery
    // regenerates them (`publish` demands them in dist/). The twins' alias sidecars
    // restate the same digests under the alias names, so `shasum -a 256 -c` works on
    // what the `releases/latest/download` URLs save.
    let stable_dmg_name = channel::stable_dmg_asset_name();
    let stable_zip_name = channel::stable_zip_asset_name();
    let sidecar_records = [
        (manifest.dmg.as_str(), manifest.sha256.as_str()),
        (recovered_zip.as_str(), recovered_zip_sha256.as_str()),
        (stable_dmg_name.as_str(), manifest.sha256.as_str()),
        (stable_zip_name.as_str(), recovered_zip_sha256.as_str()),
    ];
    for (name, sha) in sidecar_records {
        let sidecar = channel::sha256_sidecar_name(name);
        fs::write(
            dist.join(&sidecar),
            channel::sha256_sidecar_contents(sha, name),
        )
        .map_err(|error| Error::new(format!("reconstruct {sidecar}: {error}")))?;
    }
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
    reconstruct_roster_assets(slug, release_object.id, &names, &manifest, &dist)?;
    let journal_path = dist.join("cut-state.toml");
    let journal = Journal {
        format: JOURNAL_FORMAT,
        version: version.to_string(),
        build_number: build,
        commit: owner.to_string(),
        min_build: manifest.min_build,
        // Published already: nothing is left to sign.
        artifact: None,
        macho_sha256: None,
        linux: None,
        manifest_signed: signature_policy.required,
        signature_required: signature_policy.required,
        signature_pubkey: signature_policy.pubkey.clone(),
        // From the release: the recovered artifacts may have been signed by another
        // machine. `signature_pubkey` stays this machine's own key so the local guards
        // keep comparing like with like.
        verify_pubkey: recovered_pubkey.clone(),
        // From the published bytes: `machine_id` is inside the signature, so deriving
        // it locally would let a recovery relabel someone else's release.
        signature_machine_id: manifest.machine_id.clone(),
        // The published release IS the object: bound, with the durable intent that
        // binding requires. No upload is in flight — every asset is already there.
        release_id: Some(release_object.id),
        release_intent: true,
        upload_intents: Vec::new(),
        done: ["lock", "sign", "selfcheck", "tag"]
            .into_iter()
            .map(str::to_string)
            .collect(),
    };
    journal.save(&journal_path)?;
    step(
        "recover",
        &format!(
            "validated published {tag} version/build/commit + manifest{} + DMG digest; \
             reconstructed journal at publish",
            if signature_policy.required {
                "/signature/public-key"
            } else {
                ""
            },
        ),
    );
    let mut ctx = CutCtx {
        credentials: credentials.cloned(),
        // Already published: `sign` is journaled and will not run, so resolving the tier
        // could only fail the recovery on, say, a certificate expired since the cut.
        apple: sign::AppleTier::Inactive,
        repo: repo.to_path_buf(),
        // Nothing from `publish` on reads a tree, and a recovered cut starts there.
        tree: repo.to_path_buf(),
        dist,
        journal_path,
        slug: slug.to_string(),
        version: version.to_string(),
        tag,
        build,
        commit: owner.to_string(),
        min_build: manifest.min_build,
        artifact: None,
        macho_sha256: None,
        manifest_signed: signature_policy.required,
        linux: None,
        signature_required: signature_policy.required,
        signature_pubkey: signature_policy.pubkey.clone(),
        verify_pubkey: recovered_pubkey.clone(),
        signature_machine_id: manifest.machine_id.clone(),
        // Nothing left to stamp or stage: the manifest and roster assets are already
        // published bytes.
        attribution: None,
        roster: None,
        release_id: Some(release_object.id),
        release_intent: true,
        upload_intents: Vec::new(),
        kind: CutKind::Real,
        lease: Some(lease),
        fence: Some(fence),
        notes_section: version.to_string(),
        journal: Some(journal),
    };
    run_pipeline(&mut ctx, Instant::now())
}

/// The transcript's `signature` line: the key this cut actually signs with, and the
/// machine the master-signed roster says that key is: only facts the gate decided.
#[must_use]
pub fn signature_transcript_line(signing_key: Option<&str>, machine_id: Option<&str>) -> String {
    match (signing_key, machine_id) {
        (Some(key), Some(machine)) => {
            format!("signing key {key} — roster machine {machine}, authorized by the paper master")
        }
        (Some(key), None) => {
            format!("signing key {key} · no paper master pinned (a fork: no client verifies it)")
        }
        (None, _) => "unsigned — no paper master pinned and no signing configuration".to_string(),
    }
}

/// What a REAL cut publishes, and whether its claim is a recut: the READER half of
/// the claim contract (`aterm_models_release::release_claim_landing_model`; Tier-1 in
/// tests/it/claim_landing_model.rs), whose writer is [`ledger::claim`] with
/// [`changelog::claim_changelogs`]. `source` is the published commit's changelog, `main`
/// origin/main's; `published` is asked only when either carries a version section. The
/// recut signal is read from `main`, where an earlier claim rolled the section: the
/// published commit never carries it.
///
/// # Errors
/// The already-published refusal, and `published`'s errors.
pub fn real_cut_version(
    source: &str,
    main: &str,
    release_version: &str,
    published: &mut dyn FnMut(&str) -> Result<bool>,
) -> Result<(String, bool)> {
    let has_section = changelog::has_section(source, release_version)
        || changelog::has_section(main, release_version);
    let released = has_section && published(release_version)?;
    let state = verify::RemoteState {
        current_version: release_version.to_string(),
        changelog_has_section: has_section,
        published: released,
    };
    let version = match verify::derive_cut_mode(&state)? {
        verify::CutMode::Fresh { version } | verify::CutMode::Recut { version } => version,
    };
    let recut = changelog::has_section(main, &version);
    Ok((version, recut))
}

/// A dry run's statement of the commit a REAL cut would release — the
/// engine ledger's newest aterm row, read and never acted on — so the preflight
/// exercises the lookup the real cut depends on. An unreadable ledger is said, not
/// refused: this run signs the release bundle of the checkout's commit either way.
#[must_use]
pub fn dry_run_source_line(published: &Result<gates::PublishedSource>) -> String {
    match published {
        Ok(source) => format!(
            "a real cut signs its release's verified bundle of the published commit {} \
             (verified {}); this run signs its release's verified bundle of the checkout's \
             commit",
            source.commit.get(..12).unwrap_or(&source.commit),
            source.verified_at
        ),
        Err(error) => format!(
            "a real cut would REFUSE here — {error}; this run signs its release's verified \
             bundle of the checkout's commit"
        ),
    }
}

/// What a fresh cut (not `--resume`) does with a journal already on disk, of any
/// format, read by its [`JournalHeader`]: `Ok(true)`, a finished cut's history to
/// clear; `Ok(false)`, none; `Err`, a cut in flight that this one must not overrun.
/// Split out of [`run_cut`] so "a finished journal never blocks the next cut" is a test.
pub fn fresh_cut_journal_triage(existing: Option<&JournalHeader>, kind: CutKind) -> Result<bool> {
    let Some(j) = existing else {
        return Ok(false);
    };
    if j.finished() {
        return Ok(true);
    }
    let at = j.position();
    match kind {
        CutKind::Real => Err(Error::new(format!(
            "a cut is already in progress: v{} (build {}) is journaled {at} — finish it with \
             `{CUT_COMMAND}`, discard it with `{CUT_COMMAND} --abandon v{}`, or \
             delete dist/cut-state.toml",
            j.version, j.build_number, j.version
        ))),
        CutKind::DryRun => {
            // A dry run never touches the journal itself — but it restages
            // dist/ IN PLACE under a provisional number, into the very paths
            // the journaled cut's remaining steps will upload. A later
            // --resume would then ship a MIXED asset set (the real cut's DMG
            // next to a provisional-number manifest) and flip a
            // self-inconsistent release live. Refuse while a real cut is in
            // flight.
            Err(Error::new(format!(
                "an unfinished real cut is journaled: v{} (build {}) {at} — a dry-run would \
                 overwrite its dist/ artifacts with provisional-number ones; finish it \
                 (`{CUT_COMMAND}`) or discard it (`{CUT_COMMAND} --abandon v{}`) first",
                j.version, j.build_number, j.version
            )))
        }
    }
}

/// Put the cut tree at the claim commit `header` names — where a resume finishes the
/// cut, by the claim's own cutter ([`gates::own_cutter`]).
///
/// # Errors
/// A claim that is not a full commit id, and [`gates::place_cut_tree`]'s.
pub fn place_claim_tree(
    operator: &dyn GitRunner,
    repo: &Path,
    header: &JournalHeader,
) -> Result<PathBuf> {
    if !valid_lease_owner(&header.commit) {
        return Err(Error::new(format!(
            "the journaled claim {:?} is not a full 40- or 64-hex commit id",
            header.commit
        )));
    }
    Ok(gates::place_cut_tree(operator, repo, &header.commit)?.path)
}

/// The bundle a real cut signs: its release's reservation, and the complete, verified
/// bundle the release built under it.
pub struct VerifiedBundle {
    /// What the claim lands: the number and release commit the bytes carry.
    pub reserved: ledger::Reserved,
    /// The release cache's record of the bytes, re-proved on read.
    pub complete: artifact::Complete,
    /// The verified judge's words for them.
    pub line: String,
}

/// THE VERIFIED BUNDLE OF THE RELEASE OF `source`, read from
/// this clone's release journal (`ship release`'s, in the git common dir): its judgment
/// of R's commit-only gates (the `commit-gates` fact), its
/// reservation of `version`, the bundle it built under that reservation — COMPLETE in
/// the release cache, its bytes re-proved against their record — compiled by the
/// prover `toolchain` names (the [`gates::cut_toolchain`] identity: ONE compiler, named by
/// the bytes, built them), and the record that `verify release` passed for `source`'s tree on that
/// bundle's executable ([`gates::verified`]). Every refusal is pre-claim: nothing is
/// burned.
///
/// # Errors
/// No release of `source` journaled here, R's commit-only gates not judged by it, no
/// reservation or bundle, a bundle that is
/// not the reservation's or whose bytes changed, bytes another prover compiled, and
/// bytes `verify release` has not passed.
pub fn verified_release_bundle(
    operator: &dyn GitRunner,
    repo: &Path,
    source: &str,
    version: &str,
    toolchain: &str,
) -> Result<VerifiedBundle> {
    let refuse = |why: String| {
        Error::new(format!(
            "{why} — a cut (a dry run too) signs only the bundle its \
             release built once and `verify release` passed, and builds nothing itself: \
             `{}` reserves, builds, verifies and cuts. Nothing was claimed",
            crate::release::RELEASE_COMMAND
        ))
    };
    let short = |sha: &str| sha.get(..12).unwrap_or(sha).to_string();
    let journal = crate::release::Journal::load(
        &crate::release::release_dir(operator)?.join(crate::release::JOURNAL_FILE),
    )?;
    let Some(journal) = journal.filter(|j| j.commit == source) else {
        return Err(refuse(format!(
            "this clone's release journal names no release of the published commit {}",
            short(source)
        )));
    };
    if journal.version != version {
        return Err(refuse(format!(
            "this clone's release of {} is v{}, not v{version}",
            short(source),
            journal.version
        )));
    }
    // R's commit-only gates (the changelog, the predecessor's handoff fixtures, the
    // handoff policy, the locked metadata) are the release's, judged at its step 4 and
    // journaled for R: the cut reads that fact, never re-derives it, and refuses without
    // it — a cut by hand fails closed rather than skipping them.
    if !journal.has(crate::release::COMMIT_GATES) {
        return Err(refuse(format!(
            "the release of {} has not judged R's commit-only gates (the changelog, the \
             predecessor's handoff fixtures, the handoff policy, the locked metadata) \
             (`{}`): its journal holds no such fact for it",
            short(source),
            crate::release::COMMIT_GATES
        )));
    }
    let (Some(reserved), Some(built)) = (journal.reserved, journal.artifact) else {
        return Err(refuse(format!(
            "the release of {} has not reserved and built its bundle yet",
            short(source)
        )));
    };
    let store = artifact::store(repo).map_err(Error::new)?;
    let complete = artifact::read(&store, &built.key)
        .map_err(&refuse)?
        .ok_or_else(|| {
            refuse(format!(
                "the release cache ({}) holds no COMPLETE bundle {}",
                store.display(),
                short(&built.key)
            ))
        })?;
    let identity = artifact::Identity {
        version: version.to_string(),
        build: reserved.build,
        commit: reserved.commit.clone(),
    };
    if !complete.is(&identity) || complete.macho_sha256 != built.macho_sha256 {
        return Err(refuse(format!(
            "the release cache's bundle {} is build {} from {} (executable {}), not the \
             reserved build {} from {} the journal names (executable {})",
            short(&built.key),
            complete.build,
            short(&complete.commit),
            short(&complete.macho_sha256),
            reserved.build,
            short(&reserved.commit),
            short(&built.macho_sha256)
        )));
    }
    let prover = gates::toolchain_trustc(toolchain);
    if complete.trustc != prover {
        return Err(refuse(format!(
            "the bundle was compiled by trustc {}, and this cut names trustc {} ({toolchain}) \
             — the compiler of the bytes it signs and the compiler it names must be one \
             compiler. Run the cut under the prover that built them, as `{}` does (its \
             journal's {})",
            short(&complete.trustc),
            short(prover),
            crate::release::RELEASE_COMMAND,
            journal.prover
        )));
    }
    let (yes, line) = gates::verified(operator, source, &complete.macho_sha256)?;
    if !yes {
        return Err(refuse(format!("NOTHING SHIPS UNVERIFIED: {line}")));
    }
    Ok(VerifiedBundle {
        reserved: reserved.ledger(),
        complete,
        line,
    })
}

/// The whole `targo --unverified ship cut` (spec §7 order): gates → claim →
/// sign+package the verified bundle → self-check → origin tag → the one publication.
/// The version is `[workspace.package] version` as written
/// ([`release_version_from_workspace`], which refuses a non-zero patch) — NOT from
/// the ledger, which supplies only the build number. Cutting twice without
/// bumping Cargo.toml therefore lands on the already-published guard in
/// [`verify::derive_cut_mode`], which names the bump.
///
/// `repo` is the operator's checkout: `dist/` and the journal live there (the release's
/// records are the repository's, shared by every worktree, and the
/// release cache is the clone's), and the cut never moves it. Every cut signs the bundle
/// its release built once and `verify release` passed ([`verified_release_bundle`]) and builds
/// nothing: a real cut, the release of the published commit, read in the cut tree
/// ([`gates::place_published`]); a dry run, the release of `repo`'s own
/// commit, under its reserved number, claimed nowhere.
pub fn run_cut(repo: &Path, opts: &CutOptions) -> Result<()> {
    // The toolchain this cut pins is leased while it runs (`gates::arm_toolchain_lease`),
    // so the package manager never reclaims or re-lays it between two steps.
    gates::arm_toolchain_lease();
    // Resolved once, here: the explicit flag when given, else this machine's
    // provisioned identity (`~/.aterm/machine.key`). Every later stage reads this
    // value rather than re-discovering credentials, so a cut cannot change identity
    // halfway through.
    let credentials = sign::ReleaseCredentials::resolve(opts.release_credentials.as_deref(), repo)
        .map_err(Error::new)?;
    let credentials = credentials.as_ref();
    let t0 = Instant::now();
    let dist = repo.join("dist");
    let journal_path = dist.join("cut-state.toml");
    let operator = GitCli::new(repo);

    let kind = if opts.dry_run {
        CutKind::DryRun
    } else {
        CutKind::Real
    };
    if kind == CutKind::Real {
        assert_origin_repo_binding(&operator, &workspace_repo_slug(repo)?)?;
    }

    // ---- journal triage (before anything else) ----------------------------
    // By the header every format writes: who may act on a journal is decided before
    // anything reads a step of it.
    let existing = JournalHeader::read(&journal_path)?;
    if opts.resume {
        let Some(header) = existing else {
            // The journal is this checkout's; the lock is the channel's. A cut journaled
            // elsewhere — another checkout, another machine — holds the lock, and the
            // recover command needs its exact claim, which the lock names.
            let lock = match release_lease_owner(&operator) {
                Ok(Some(owner)) => format!(
                    "the release lock is held by claim {owner}; once that cut is stopped: `{}`",
                    fence_recover_command(None, &owner)
                ),
                Ok(None) => "no release lock is held".to_string(),
                Err(error) => format!("the release lock could not be read: {error}"),
            };
            return Err(Error::new(format!(
                "nothing to resume: no cut is journaled in this checkout (dist/cut-state.toml); \
                 {lock}"
            )));
        };
        if kind != CutKind::Real {
            return Err(Error::new(
                "--resume applies to a real cut only (a dry run is never journaled)".to_string(),
            ));
        }
        // A resume finishes the release commit: the cut tree goes back there, and that
        // commit's own cutter finishes it, whatever format it journals in.
        let tree = place_claim_tree(&operator, repo, &header)?;
        gates::own_cutter_gate(&GitCli::new(&tree), &header.commit, false)?;
        let j = Journal::load(&journal_path)?.ok_or_else(|| {
            Error::new(format!(
                "{} vanished while resuming",
                journal_path.display()
            ))
        })?;
        let slug = workspace_channel_slug(&tree)?;
        return resume_cut(
            ResumePaths {
                repo,
                tree: &tree,
                dist: &dist,
                journal_path: &journal_path,
            },
            &slug,
            j,
            t0,
            None,
            credentials,
        );
    }
    if fresh_cut_journal_triage(existing.as_ref(), kind)? {
        // A finished cut's journal — whichever cutter wrote it — is just history: clear
        // it and move on.
        let _ = fs::remove_file(&journal_path);
    }
    // ---- the published commit (before the tier, the version and the claim) -
    // THE CUT RELEASES WHAT `pub publish` PUBLISHED (owner ruling R2): the
    // newest aterm row of the engine's ledger, whatever main's tip is, in the cut
    // tree — this checkout never moves. When this binary is not that commit's own
    // cutter, the cut is handed to the one built there.
    //
    // WHERE IT SITS IS THE DESIGN. After the journal triage, because a cut that is
    // already journaled must refuse without having moved anything first — a resume
    // is bound to its release commit. Before everything else, because every
    // decision below reads the tree it places. A dry run signs the release bundle of
    // the checkout's commit, and says which commit a real cut would release.
    let published = if kind == CutKind::Real {
        let source = gates::published_source()?;
        let checkout = gates::place_published(&operator, repo, &source)?;
        step(
            "source",
            &format!(
                "{} {} at the published commit {} (verified {}) — main is {} commit(s) past \
                 it, and none of them ships in this cut; this checkout is not touched",
                if checkout.moved { "placed" } else { "kept" },
                checkout.tree.display(),
                &source.commit[..12],
                source.verified_at,
                checkout.main_ahead
            ),
        );
        gates::own_cutter_gate(&GitCli::new(&checkout.tree), &source.commit, false)?;
        Some(checkout)
    } else {
        step("source", &dry_run_source_line(&gates::published_source()));
        None
    };
    // Everything below reads the tree the cut releases.
    let tree = published
        .as_ref()
        .map_or_else(|| repo.to_path_buf(), |checkout| checkout.tree.clone());
    let git = GitCli::new(&tree);
    let cargo_text = fs::read_to_string(tree.join("Cargo.toml"))
        .map_err(|e| Error::new(format!("read {}: {e}", tree.join("Cargo.toml").display())))?;
    let full = workspace_version(&cargo_text)?;
    let origin_slug = repo_slug(&cargo_text).ok_or_else(|| {
        Error::new(
            "Cargo.toml [workspace.package] repository is not an exact GitHub OWNER/REPO URL",
        )
    })?;
    if kind == CutKind::Real {
        // The tree's own statement of its repository, bound again: the one read
        // before the placement was this checkout's.
        assert_origin_repo_binding(&git, &origin_slug)?;
    }
    // THE release channel: the same tracked key `aterm-update-core/build.rs` compiles
    // into every client, so the cut publishes exactly where the fleet looks.
    let publish_slug = channel_slug(&cargo_text)?;
    // Every `gh` call a real cut makes from here on is a release-channel call.
    let _cred = (kind == CutKind::Real).then(ChannelCred::enter);
    // The version this cut publishes: the workspace version as written. The ledger
    // supplies only the build number (below).
    let release_version = release_version_from_workspace(&full)?;
    // Recorded into any fence this process creates, and the fallback version a refusal
    // prints for a fence that records none. A resume overrides it with the journal's.
    set_publisher_fence_version(&release_version);

    // Tier APPLE, resolved here: after the resume delegation (a resume resolves its
    // own, `resume_apple_tier`) and before the gates, the lease and the claim, so a
    // missing Developer-ID certificate or notarytool credential fails before the claim
    // burns a build number (docs/RELEASE-KEYS.md's ordering rule).
    let linux = opts
        .linux_artifacts
        .as_deref()
        .map(|directory| {
            buildplan::linux::Handoff::new(directory, &opts.linux_targets, &opts.linux_workers)
        })
        .transpose()?;
    let apple = resolve_apple_tier(aterm_update_core::pins::APPLE_TEAM_ID, credentials)?;

    // ---- decide the version (fresh vs remote-derived recut, spec §5) ------
    let changelog_text = fs::read_to_string(tree.join(changelog::CHANGELOG_FILE))
        .map_err(|e| Error::new(format!("read {}: {e}", changelog::CHANGELOG_FILE)))?;
    // The notes: the published commit's changelog is what ships. Main's matters only
    // for a section an earlier claim of this version already rolled there, a wedged
    // cut this one re-claims ([`ledger::ClaimPlan`]).
    let main_changelog = if kind == CutKind::Real {
        String::from_utf8(
            git_ok(
                &git,
                &[
                    "show",
                    &format!("origin/main:{}", changelog::CHANGELOG_FILE),
                ],
            )?
            .stdout,
        )
        .map_err(|_| Error::new("origin/main's CHANGELOG.md is not valid UTF-8"))?
    } else {
        String::new()
    };
    let (version, recut) = if kind == CutKind::Real {
        real_cut_version(
            &changelog_text,
            &main_changelog,
            &release_version,
            &mut |version| {
                Ok(
                    verify::release_state(&publish_slug, &format!("v{version}"))?
                        == verify::ReleaseState::Published,
                )
            },
        )?
    } else {
        // A dry run never rolls, so there is no recut concept: the version
        // is the workspace's; notes come from [Unreleased].
        (release_version.clone(), false)
    };
    ledger::check_version_shape(&version)?;

    let head8: String = rev_parse(&git, "HEAD")?.chars().take(8).collect();
    let flavor = match kind {
        CutKind::Real if recut => " [recut]",
        CutKind::Real => "",
        CutKind::DryRun => " [dry-run]",
    };
    println!("aterm-release · cut v{version} ({head8}){flavor}");

    // ---- gates (spec §6; <5s, before anything is committed) ---------------
    let gate_opts = gates::GateOpts {
        version: version.clone(),
        // Only a REAL cut is compared against the public channel: a dry run
        // uploads nothing, so the channel cannot be expected to carry this
        // version. This is the sole opt-out and it is structural — derived from
        // the flags, never readable from the environment.
        offline: !matches!(kind, CutKind::Real),
        allow_stale_cutter: matches!(kind, CutKind::DryRun),
        published: published.clone(),
    };
    let gr = gates::run_all(&git, &tree, repo, &gate_opts)?;
    step(
        "gates",
        &format!(
            "clean tree · HEAD {} ({}) · tag v{version} free (local+remote)",
            if gr.published.is_some() {
                "== the published commit"
            } else {
                "as checked out"
            },
            gr.head_short
        ),
    );

    step(
        "",
        &format!(
            "gh auth ({}) · disk ok ({} GiB free)",
            gr.gh_account.as_deref().unwrap_or("account unknown"),
            gr.free_disk_gib,
        ),
    );
    step(
        "",
        &match (gr.channel_version.as_deref(), gate_opts.offline) {
            (Some(v), _) => format!("public channel source agrees: carries {v}"),
            (None, true) => {
                "public channel: not compared (this run publishes nothing there)".to_string()
            }
            (None, false) => "public channel: no source version to compare".to_string(),
        },
    );
    step(
        "",
        &format!(
            "no process runs out of a cut staging bundle under dist/ ({} processes read)",
            gr.processes_checked
        ),
    );
    // One compiler, named by the bytes: the toolchain this cut names is the one whose
    // trustc compiled the bundle it signs (refused otherwise).
    let toolchain = gates::cut_toolchain()
        .map_err(|e| Error::new(format!("the cut cannot name its prover: {e}")))?;
    // NOTHING SHIPS UNVERIFIED, AND NOTHING IS BUILT TWICE: every cut signs the bundle
    // its release built once, under the number it reserved, and `verify release` passed
    // — this clone's release journal names it, the release cache holds it COMPLETE with
    // its bytes re-proved, and the verified record of the tree names its executable. A
    // real cut's release is of the published commit; a dry run's is of the
    // checkout's commit, and its reserved number is claimed nowhere. Pre-claim, so a refusal burns nothing.
    let source = match &published {
        Some(checkout) => checkout.source.commit.clone(),
        None => rev_parse(&git, "HEAD")?,
    };
    let verified = verified_release_bundle(&operator, repo, &source, &version, &toolchain)?;
    // WHAT DEPENDS ONLY ON THE TREE IS READ, NEVER RE-DERIVED: R's commit-only gates
    // are the release's journaled judgment (`verified_release_bundle` refused without
    // it), and the handoff fixture guard and the L0 censuses are in the `release` set
    // whose pass record for R's tree that judge just read.
    step(
        "gate",
        &format!(
            "R's commit-only gates (the changelog, the predecessor's handoff fixtures, the \
             handoff policy, the locked metadata): judged by its release before anything \
             was staged (`{}` in this clone's release journal); the handoff fixture guard \
             and the L0 censuses passed in `verify release` over its tree",
            crate::release::COMMIT_GATES,
        ),
    );
    // Two of them also read origin, so they are asked again here: R must hold the
    // handoff fixtures of the newest release tagged on origin, and its sealed policy is
    // judged against that release's. A version tagged since the release judged them (a
    // peer finishing a wedged cut during the long steps) is a new predecessor whose hop
    // would otherwise go unguarded. Pre-claim.
    let fixtures = gates::handoff_fixture_gate_at(&git, &source, &version)?;
    step("gate", &gates::handoff_fixtures_line(fixtures.as_ref()));
    let policy = gates::handoff_policy_gate_at(&git, &source, &version)?;
    step(
        "gate",
        &format!("handoff policy sealed into the bundle: {policy}"),
    );
    step("gate", &verified.line);
    if kind != CutKind::Real && !opts.linux_workers.is_empty() {
        // A Linux worker fetches the commit it builds from origin, and only a real
        // claim puts the release commit there.
        return Err(Error::new(format!(
            "a dry run signs its release's reserved commit {}, which is pushed \
             nowhere until a real claim lands it, so a Linux worker (it fetches the commit it \
             builds from origin) cannot build it — pass --mac-only, or --linux-artifacts \
             holding pairs built from that commit by hand (`{SHIP_COMMAND} linux-build \
             --commit {}` on each Linux host that has it)",
            &verified.reserved.commit[..12],
            verified.reserved.commit
        )));
    }
    if kind == CutKind::Real {
        preflight_release_lease(&git)?;
        step("lock", "release lock is free");
    }
    if kind == CutKind::Real {
        // Prove the channel is public and writable BEFORE the ledger claim — with the
        // credential `publish` will use (the release-org token). Failing at `publish`
        // would burn a build number, and no amount of `--resume` fixes a missing
        // permission grant.
        preflight_channel_target(&publish_slug, false)?;
        step(
            "channel",
            &format!("release channel {publish_slug} is public and writable (pre-claim)"),
        );
    }

    // ---- channel floor (before claim: bad input must not burn a number) ----
    // The updater selects the first valid manifest on GitHub's newest-first
    // release stream. Its floor is channel state, not a one-cut CLI option:
    // every successor must carry it forward or a fresh client could forget a
    // prior yank. The late lock/selfcheck/publish scans repeat this guard to
    // close the race with another publisher. This read is held to the latest
    // pointer, so a torn listing refuses here rather than burning the claim.
    let head_scan = verify::scan_head_before_claim(&publish_slug)?;
    let newest_channel = head_scan.first();
    let newest_min_build = newest_channel.and_then(|published| published.min_build);
    // The machine-roster gate, the last pre-claim gate: a refusal after the claim burns
    // a single-use build number, and everything it needs is local (a file, a
    // signature, a clock).
    let signature_verdict = preflight_signature_policy(credentials, RosterDuty::Sign)
        .map_err(|e| Error::new(format!("{}; nothing was claimed", e.0)))?;
    let signature_policy = signature_verdict.policy.clone();
    step(
        "signature",
        &signature_transcript_line(
            signature_policy.pubkey.as_deref(),
            signature_verdict.machine_id().as_deref(),
        ),
    );
    // THE ROSTER RATCHET, pre-claim, against the head this scan already has in hand.
    // `machines::authorize_cut` judges the roster document; only this can judge the
    // roster GENERATION, because the floor is channel state and lives in the published
    // head's manifest. Both refusals must land before the claim: a cut whose roster is
    // older than the channel's is one the fleet refuses on sight, and finding that out
    // after burning a build number costs a number that can never be reused.
    // The floor the FLEET actually holds is the roster GENERATION it has observed on the
    // public channel's latest release — the master-admitted `aterm-machines.toml` asset —
    // which can run AHEAD of the head manifest's own attribution: another machine may
    // join the roster and attach the new pair to already-published releases (measured
    // 2026-08-18: v0.23.0/v0.24.0 said `roster_seq = 2`, their roster asset said 3, and
    // every client refused the cut this gate had passed with `SeqMismatch`). So the
    // ratchet reads BOTH and takes the greater. Only a real cut has a public channel to
    // ask; a dry run keeps the manifest-only floor.
    let manifest_roster_seq = published_roster_seq(newest_channel)?;
    let observed_roster = if kind == CutKind::Real {
        machines::channel_roster_document(&publish_slug).map_err(|e| {
            Error::new(format!(
                "cannot read the machine roster on the channel {publish_slug}'s latest \
                 release ({e}); refusing to reason about the fleet's roster floor — a wrong \
                 answer here burns a build number and strands every client"
            ))
        })?
    } else {
        None
    };
    let observed_roster_seq = observed_roster.as_ref().map(|(seq, _)| *seq);
    // Equal generation, different document is a lineage fork: only the bytes can
    // refuse it, and before the claim. Compared with the bytes that authorized this cut
    // and will ship, not whatever dist/ holds.
    if let Some(document) = signature_verdict.roster.as_ref() {
        machines::roster_lineage_agrees(
            &document.bytes,
            signature_verdict
                .attribution
                .as_ref()
                .map(|who| who.roster_seq),
            observed_roster.as_ref(),
        )
        .map_err(Error::new)?;
    }
    let newest_roster_seq = match (manifest_roster_seq, observed_roster_seq) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    if let (Some(observed), Some(manifest)) = (observed_roster_seq, manifest_roster_seq)
        && observed > manifest
    {
        step(
            "roster",
            &format!(
                "the public channel head carries roster generation {observed} while its \
                 manifest was attributed under {manifest} — the fleet's floor is {observed}"
            ),
        );
    }
    roster_floor_covered(
        signature_verdict
            .attribution
            .as_ref()
            .map(|who| who.roster_seq),
        newest_roster_seq,
    )?;
    // Announced only when the tier is armed: an inert anchor adds no transcript
    // lines, as Tier APPLE does.
    if let Some(who) = &signature_verdict.attribution {
        step(
            "roster",
            &format!(
                "roster generation {} (channel head {})",
                who.roster_seq,
                newest_roster_seq.map_or_else(|| "none".to_string(), |seq| seq.to_string())
            ),
        );
    }
    let attributed = signature_verdict.cut_attribution();

    // ---- claim (spec §2) ---------------------------------------------------
    let now = unix_now();
    // A real cut claims against origin/main's ledger; the published commit's may
    // be behind it.
    let ledger_text = if kind == CutKind::Real {
        ledger::show_origin_ledger(&git)?
    } else {
        fs::read_to_string(tree.join(ledger::LEDGER_FILE))
            .map_err(|e| Error::new(format!("read {}: {e}", ledger::LEDGER_FILE)))?
    };
    let tail = ledger::tail(&ledger_text)?;
    // Every cut's number is its release's reservation — the number its verified bytes
    // carry — and the claim never re-numbers it.
    let reserved = verified.reserved.build;
    let provisional_floor = effective_min_build(opts.min_build, newest_min_build, reserved)?;
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
                    "ledger tail {} ({}) → claiming the reserved {reserved} (the number the \
                     verified bytes carry; never re-numbered)",
                    tail.build, tail.version
                ),
            );
            let plan = ledger::ClaimPlan {
                version: &version,
                now,
                allow_existing_section: recut,
                max_attempts: ledger::MAX_CLAIM_ATTEMPTS,
                source: &source,
            };
            let date = changelog::today_la()?;
            let rolls = |source: &str, main: &str| {
                changelog::claim_changelogs(source, main, &version, &date)
            };
            let claim = ledger::claim(&git, &tree, &plan, &verified.reserved, &rolls)?;
            step(
                "",
                &format!(
                    "pushed \"release: v{version} (build {})\" — the release commit {} is the \
                     published {} plus the ledger line and the rolled changelog; main took it \
                     {}  [verified: origin/main == {}, ledger tail == \"{}\"]",
                    claim.build,
                    &claim.commit[..12],
                    &source[..12],
                    if claim.landed == claim.commit {
                        "as a fast-forward".to_string()
                    } else {
                        format!("by the merge {}", &claim.landed[..12])
                    },
                    &claim.landed[..12],
                    claim.ledger_line
                ),
            );
            (claim.build, claim.commit)
        }
        CutKind::DryRun => {
            // The reservation stays provisional: claimed nowhere, never pushed.
            step(
                "claim",
                &format!(
                    "ledger tail {} ({}) → the reserved {reserved}, claimed nowhere (no ledger \
                     push — dry-run)",
                    tail.build, tail.version
                ),
            );
            (reserved, verified.reserved.commit.clone())
        }
    };
    // The claim lands the reservation's number or refuses; the journal and manifest
    // are still bound to the number really won.
    let min_build = effective_min_build(opts.min_build, newest_min_build, build)?;

    let mut ctx = CutCtx {
        credentials: credentials.cloned(),
        apple,
        repo: repo.to_path_buf(),
        tree,
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
        artifact: Some(verified.complete.key.clone()),
        macho_sha256: Some(verified.complete.macho_sha256.clone()),
        linux,
        manifest_signed: false,
        signature_required: signature_policy.required,
        signature_pubkey: signature_policy.pubkey,
        // This machine signs and this machine verifies: one key, no split.
        verify_pubkey: None,
        signature_machine_id: attributed.machine_id,
        attribution: attributed.attribution,
        roster: attributed.roster,
        release_id: None,
        release_intent: false,
        upload_intents: Vec::new(),
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
            artifact: ctx.artifact.clone(),
            macho_sha256: ctx.macho_sha256.clone(),
            linux: ctx.linux.clone(),
            manifest_signed: ctx.manifest_signed,
            signature_required: ctx.signature_required,
            signature_pubkey: ctx.signature_pubkey.clone(),
            verify_pubkey: ctx.verify_pubkey.clone(),
            signature_machine_id: ctx.signature_machine_id.clone(),
            release_id: None,
            release_intent: false,
            upload_intents: Vec::new(),
            done: vec![],
        };
        j.save(&journal_path)?;
        ctx.journal = Some(j);
    }

    run_pipeline(&mut ctx, t0)
}

/// The paths a resume works over. `tree` is the cut tree, already at the journaled
/// release commit ([`gates::place_cut_tree`]); the other three are the operator's
/// checkout.
struct ResumePaths<'a> {
    repo: &'a Path,
    tree: &'a Path,
    dist: &'a Path,
    journal_path: &'a Path,
}

/// `--resume`: rebuild the context from the journal and re-enter at the first
/// incomplete step (spec §5).
fn resume_cut(
    paths: ResumePaths<'_>,
    slug: &str,
    journal: Journal,
    t0: Instant,
    recovered_session: Option<(ReleaseLeaseGuard, PublisherFenceGuard)>,
    credentials: Option<&sign::ReleaseCredentials>,
) -> Result<()> {
    let ResumePaths {
        repo,
        tree,
        dist,
        journal_path,
    } = paths;
    let Some(next) = journal.first_incomplete() else {
        // The release is complete; the site follows in `ship release`'s after-cut step.
        return Err(Error::new(format!(
            "the journaled cut already completed every step — nothing to resume \
             (delete dist/cut-state.toml, or just cut: a fresh cut clears it). The website \
             follows the release in its after-cut step: `{}`",
            crate::release::RELEASE_COMMAND
        )));
    };
    let git = GitCli::new(tree);
    // The journal's version, not the workspace's, is what this invocation publishes:
    // any fence this resume creates records it, and a refusal falls back to it.
    set_publisher_fence_version(&journal.version);
    println!(
        "aterm-release · cut v{} (build {}) — RESUME at step \"{next}\"",
        journal.version, journal.build_number
    );

    // A journal is a crash cursor, never publication authority: bind every resume to
    // its exact claim-commit ledger tail and origin/main, and reject any worktree change
    // before acquiring a remote lease or fence. The worktree is the cut tree at the claim.
    ordinary_resume_claim_preflight(tree, &git, &journal)?;

    // A killed resume leaves its fence behind; this resume takes it back only on proof
    // that the recorded publisher is dead (`reclaim_dead_publisher_fence`), and anything
    // unprovable is refused below with the recover command. A recovery arrives with its
    // session already rotated under its own operator proof. Skipped for an unlock-only
    // resume, which acquires no fence (the predicate is `run_pipeline`'s): there a
    // remote fence can be a successor cut's, and probing it would print that cut's
    // refusal here.
    if recovered_session.is_none() && next != "unlock" {
        match reclaim_dead_publisher_fence(&git, &journal.commit, &LocalProbe) {
            Ok(FenceReclaim::NoFence) => {}
            Ok(FenceReclaim::Reclaimed { token, detail }) => {
                println!("  fence: reclaimed {PUBLISHER_FENCE_REF} (token {token}) — {detail}")
            }
            Ok(FenceReclaim::Kept { refusal }) => {
                println!("  fence: held by another publisher session\n{refusal}");
            }
            // The probe is an assist, never an authority: if it cannot run, the
            // acquire below still produces the authoritative refusal.
            Err(error) => println!("  fence: liveness probe could not run: {error}"),
        }
    }

    // A resume that will still sign must hold the journaled signing key. The
    // claim-commit and clean-tree proof above applies to every resume.
    if !journal.is_done("sign") && journal.signature_required {
        let material = load_signing_material(credentials)?.ok_or_else(|| {
            Error::new(
                "signature-required resume cannot sign without the recovered offline signing configuration",
            )
        })?;
        if Some(material.pubkey.as_str()) != journal.signature_pubkey.as_deref() {
            return Err(Error::new(
                "resume signing key differs from the journaled actual channel public key",
            ));
        }
    }

    // The roster, re-derived only for a resume that will still sign (see `RosterDuty`):
    // it may have lapsed or revoked this machine since the cut began, which a journal
    // cannot know. What the journal knows is which machine started the cut, and a
    // resume authorizing as another machine is refused (`resume_attribution_agrees`, a
    // pure function so a fork, which never enters this block, still tests it).
    let resumed = if aterm_update_core::pins::roster_tier_armed() && !journal.is_done("sign") {
        let verdict = preflight_signature_policy(credentials, RosterDuty::Sign)?;
        resume_attribution_agrees(
            journal.signature_machine_id.as_deref(),
            verdict.machine_id().as_deref(),
        )?;
        Some(verdict)
    } else {
        None
    };

    let resumed_attribution =
        resumed.map_or_else(CutAttribution::none, SigningVerdict::cut_attribution);

    // Same `!is_done("sign")` rule as the key re-proof above: a resume that can still
    // sign must re-prove it can sign and notarize (a certificate can expire mid-cut),
    // and one that cannot is asked for nothing.
    let apple = resume_apple_tier(
        aterm_update_core::pins::APPLE_TEAM_ID,
        &journal,
        credentials,
    )?;

    let (lease, fence) =
        recovered_session.map_or((None, None), |(lease, fence)| (Some(lease), Some(fence)));
    let mut ctx = CutCtx {
        credentials: credentials.cloned(),
        apple,
        repo: repo.to_path_buf(),
        tree: tree.to_path_buf(),
        dist: dist.to_path_buf(),
        journal_path: journal_path.to_path_buf(),
        slug: slug.to_string(),
        version: journal.version.clone(),
        tag: format!("v{}", journal.version),
        notes_section: journal.version.clone(),
        build: journal.build_number,
        commit: journal.commit.clone(),
        min_build: journal.min_build,
        artifact: journal.artifact.clone(),
        macho_sha256: journal.macho_sha256.clone(),
        linux: journal.linux.clone(),
        manifest_signed: journal.manifest_signed,
        signature_required: journal.signature_required,
        signature_pubkey: journal.signature_pubkey.clone(),
        // A resume of a recovered cut keeps verifying under the release's key, not
        // this machine's; `None` on every ordinary journal.
        verify_pubkey: journal.verify_pubkey.clone(),
        // The id comes from the journal on every resume: it is the cut's fixed
        // identity. The attribution and roster come from the re-derivation, `None` on a
        // resume that will not sign.
        signature_machine_id: journal.signature_machine_id.clone(),
        attribution: resumed_attribution.attribution,
        roster: resumed_attribution.roster,
        release_id: journal.release_id,
        release_intent: journal.release_intent,
        upload_intents: journal.upload_intents.clone(),
        kind: CutKind::Real,
        lease,
        fence,
        journal: Some(journal),
    };
    run_pipeline(&mut ctx, t0)
}

/// Remove this claim's staging directory after a finished cut, so no launchable
/// copy of the app under the release's bundle id is left in dist/
/// ([`bundle::discard_staged_app`], under the staging liveness rule). A failure only
/// warns: the release is already published, and the next cut's prune retries.
fn discard_staged(ctx: &CutCtx) {
    let dir = bundle::staging_dir_name(ctx.build);
    match bundle::discard_staged_app(&ctx.dist, ctx.build) {
        Ok(()) => step(
            "tidy",
            &format!("removed dist/{dir}/aterm.app (the DMG and zip carry it)"),
        ),
        Err(error) => step(
            "tidy",
            &format!("WARNING: kept dist/{dir}/aterm.app: {error}"),
        ),
    }
}

/// Execute the journaled steps in order, skipping completed ones: the one pipeline
/// every cut flavor shares.
fn run_pipeline(ctx: &mut CutCtx, t0: Instant) -> Result<()> {
    // Every `gh` call a real cut's pipeline makes is a release-channel call.
    let _cred = (ctx.kind == CutKind::Real).then(ChannelCred::enter);
    // A resume may begin after selfcheck. Every suffix, a publish-only retry
    // included, must bind its native bytes to the frozen journal and signed
    // appcast before any publication operation is reachable.
    if ctx.linux.is_some()
        && ctx
            .journal
            .as_ref()
            .is_some_and(|journal| journal.is_done("sign"))
    {
        if let Some(handoff) = &ctx.linux {
            handoff.verify_staged(&ctx.dist, &ctx.version, ctx.build, &ctx.commit)?;
        }
        let manifest = Manifest::parse(
            &fs::read_to_string(ctx.manifest_path())
                .map_err(|e| Error::new(format!("read resumed appcast: {e}")))?,
        )?;
        buildplan::linux::verify_manifest_handoff(&manifest, ctx.linux.as_ref())?;
    }
    // Resume re-proves or reacquires exact ownership even when `lock` is journaled,
    // except on an unlock-only resume: absence may mean the delete landed and the
    // journal mark crashed, so reacquiring would undo convergence.
    if ctx.kind == CutKind::Real
        && !matches!(
            ctx.journal.as_ref().and_then(Journal::first_incomplete),
            Some("unlock")
        )
    {
        if ctx.lease.is_none() {
            let git = GitCli::new(&ctx.repo);
            ctx.lease = Some(acquire_release_lease(&git, &ctx.commit)?);
        }
        if ctx.fence.is_none() {
            let git = GitCli::new(&ctx.repo);
            ctx.fence = Some(acquire_publisher_fence(&git, &ctx.commit)?);
        }
        // The pre-claim read is only an early refusal. Channel signing state can advance
        // while the ledger CAS is racing, so the acquired session re-derives the policy
        // before any signing or upload is trusted.
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
            "sign" => step_sign(ctx)?,
            "selfcheck" => {
                step_selfcheck(ctx)?;
                if ctx.kind == CutKind::DryRun {
                    step(
                        "DONE",
                        &format!(
                            "dry-run: v{} (build {}) built{} and self-checked in dist/ — \
                             nothing committed or published.  [{}]",
                            ctx.version,
                            ctx.build,
                            if ctx.apple.identity().is_some() {
                                ", notarized"
                            } else {
                                ""
                            },
                            fmt_elapsed(t0)
                        ),
                    );
                    return Ok(());
                }
            }
            "tag" => step_tag(ctx)?,
            "publish" => step_publish(ctx)?,
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
        CutKind::Real => {
            discard_staged(ctx);
            // The website is `ship release`'s after-cut step, which first checks the
            // release's shape as GitHub serves it.
            step(
                "DONE",
                &real_cut_done_line(
                    &ctx.version,
                    ctx.build,
                    &fmt_elapsed(t0),
                    &ctx.commit,
                    &ctx.tree,
                ),
            );
        }
        CutKind::DryRun => unreachable!("dry-run returned after selfcheck"),
    }
    Ok(())
}

/// A real cut's closing transcript line. It states no delivery deadline: how soon
/// an install stages a release is the updater's cadence, which lives in another
/// crate and moves. It states where the cut left things — the journal, and the cut
/// tree at the release commit.
#[must_use]
pub fn real_cut_done_line(
    version: &str,
    build: u64,
    elapsed: &str,
    commit: &str,
    tree: &Path,
) -> String {
    format!(
        "v{version} (build {build}) is live — every install stages it at its next update \
         check.  [{elapsed}]  state: dist/cut-state.toml · built in {} at the release \
         commit {}",
        tree.display(),
        commit.get(..12).unwrap_or(commit)
    )
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

/// Re-derive the signing verdict (the per-machine configuration folded with the
/// committed channel pin) while the exact owner+process token is held. Equality covers
/// the canonical key, not just a boolean: a cut whose key vanished, whose signing
/// configuration appeared, or whose pin changed mid-cut aborts rather than proceed
/// under the state it claimed under. This holds the pinned-channel invariant at lock
/// and every later step, not only at the pre-claim scan.
fn revalidate_ctx_signature_policy(ctx: &CutCtx) -> Result<()> {
    if ctx.kind != CutKind::Real {
        return Ok(());
    }
    ensure_ctx_release_lease(ctx)?;
    // The duty, from the same fact `resume_cut` and `run_recover_lost` read: a post-`sign`
    // entry must not demand a still-fresh roster (see `RosterDuty`).
    let duty = roster_duty(ctx.is_done("sign"));
    let observed = preflight_signature_policy(ctx.credentials.as_ref(), duty)?;
    if observed.policy.required != ctx.signature_required
        || observed.policy.pubkey.as_deref() != ctx.signature_pubkey.as_deref()
    {
        return Err(Error::new(
            "local signing configuration or the committed channel pin changed after this \
             cut's pre-claim scan; refusing to build/upload/flip under stale signing state",
        ));
    }
    // On a sign entry the call above re-proved the roster (freshness and revocation);
    // this adds identity: the machine that will stamp and sign must be the one the
    // journal records. On a finish entry `observed.machine_id()` is `None` and nothing
    // is compared, since the attribution is inside signed bytes; that is what lets any
    // rostered machine finish a dead publisher's released cut.
    if duty == RosterDuty::Sign {
        resume_attribution_agrees(
            ctx.signature_machine_id.as_deref(),
            observed.machine_id().as_deref(),
        )?;
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
            "release lock held by claim {} · newest live build {}",
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
                 `{SHIP_COMMAND} recover v{} {} --old-publisher-stopped` for a surviving \
                 same-claim token",
                ctx.version, ctx.commit
            ))
        })?
    };
    ctx.lease = None;
    ctx.fence = None;
    step(
        "unlock",
        match outcome {
            LeaseRelease::Released | LeaseRelease::AlreadyAbsent => "release lock freed",
            LeaseRelease::AlreadySuperseded => "release lock freed; the next cut already holds it",
        },
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Notarize + package: the ordered middle of `step_sign`, extracted so the order is a
// tested fact
// ---------------------------------------------------------------------------

/// The two container builders and the two digest reads, behind a trait. `hdiutil` and
/// `ditto` want a real signed bundle and tens of seconds, and a mutation of the
/// sequence that calls them (notarizing after the zip, skipping the post-hook re-hash)
/// yields a green cut and a broken artifact; this seam lets a fake drive
/// [`notarize_and_package`] offline and record the order. The real implementation
/// delegates to `dmg.rs`.
pub trait Packager {
    fn dmg(&self, app: &Path, dist: &Path, version: &str) -> Result<dmg::Packaged>;
    fn zip(&self, app: &Path, dist: &Path, version: &str) -> Result<dmg::Packaged>;
    fn sha256(&self, path: &Path) -> Result<String>;
    fn size(&self, path: &Path) -> Result<u64>;
}

/// The real packaging tools. The only implementation the pipeline constructs.
pub struct RealPackager;

impl Packager for RealPackager {
    fn dmg(&self, app: &Path, dist: &Path, version: &str) -> Result<dmg::Packaged> {
        dmg::create(app, dist, version).map_err(Error::new)
    }
    fn zip(&self, app: &Path, dist: &Path, version: &str) -> Result<dmg::Packaged> {
        dmg::create_zip(app, dist, version).map_err(Error::new)
    }
    fn sha256(&self, path: &Path) -> Result<String> {
        dmg::sha256_file(path).map_err(Error::new)
    }
    fn size(&self, path: &Path) -> Result<u64> {
        Ok(fs::metadata(path)
            .map_err(|e| Error::new(format!("stat {}: {e}", path.display())))?
            .len())
    }
}

/// Every container, and the digests that go on record for them.
pub struct PackagedCut {
    pub dmg: dmg::Packaged,
    /// The DMG digest after the Tier APPLE hook, which rewrites the bytes.
    pub dmg_sha256: String,
    /// The DMG size after the hook, for the same reason.
    pub dmg_size: u64,
    pub zip: dmg::Packaged,
}

/// The line said before the two notarization waits: how long Apple usually takes, when
/// this cut gives up, and, on a real cut (the only journaled kind), that a stopped cut
/// continues with `--resume`.
#[must_use]
pub fn notarize_notice(kind: CutKind) -> String {
    let mut line = format!(
        "notarizing with Apple (the app, then the DMG): usually 2-10 min each, at most {} min \
         each",
        sign::NOTARY_SUBMIT_TIMEOUT.as_secs() / 60
    );
    if kind == CutKind::Real {
        line.push_str(&format!(
            "\nif it stops, `{CUT_COMMAND}` continues this cut"
        ));
    }
    line
}

/// Notarize the bundle, build both containers around it, notarize the DMG, and return
/// the digests that go on record. The order is the property; each step guards against
/// a green cut with a broken artifact:
///
/// 1. the `.app` is notarized and stapled first, because [`Packager::zip`] archives the
///    bundle as it finds it and the zip is what self-updating installs download (an
///    unstapled zip fails on any Mac that cannot reach Apple);
/// 2. the DMG is built after that staple;
/// 3. the hook Developer-ID signs and notarizes the DMG, rewriting its bytes;
/// 4. so the DMG digest is re-read afterwards, driven by what the hook reports doing.
///
/// On the inactive tier (a fork) both hooks do nothing and no re-hash happens.
pub fn notarize_and_package(
    app: &Path,
    dist: &Path,
    version: &str,
    tier: &sign::AppleTier,
    tools: &dyn sign::AppleTools,
    pack: &dyn Packager,
) -> Result<PackagedCut> {
    // The caller announces the wait (`notarize_notice`). The bundle is notarized first,
    // before either container exists.
    let notarized_app = sign::notarize_app(app, tier, tools).map_err(Error::new)?;
    if notarized_app {
        step(
            "notarize",
            &format!(
                "{} — submitted, stapled and validated before packaging",
                app.display()
            ),
        );
    }

    // The one DMG: `dmg::create`'s image of the app exactly as signed (and, on the
    // active tier, stapled), under the fleet-pinned bare `aterm-<v>.dmg` name.
    let dmg_out = pack.dmg(app, dist, version)?;
    // The hook. Inactive: returns false having done nothing. Active: Developer-ID signs,
    // preflights, notarizes and staples the DMG, and any failure aborts the cut: the
    // manifest stamps `team_id` from the anchor unconditionally, and a non-empty
    // `team_id` promises `tools/install.sh` and the in-app updater a notarized artifact.
    let dmg_notarized =
        sign::sign_and_notarize_dmg(&dmg_out.path, tier, tools).map_err(Error::new)?;
    // Re-hash after the hook: codesign rewrites the DMG and the staple appends a ticket,
    // so `dmg::create`'s digest covers the pre-hook bytes only, and the manifest must
    // carry the digest of the bytes clients download.
    let (dmg_sha256, dmg_size) = if dmg_notarized {
        (pack.sha256(&dmg_out.path)?, pack.size(&dmg_out.path)?)
    } else {
        (dmg_out.sha256.clone(), dmg_out.size_bytes)
    };
    // The updater container, from the same signed (and, on the active tier, stapled)
    // .app: `ditto` must archive the bundle directly to preserve its seal, and
    // `create_zip` hashes what it writes, so its digest already covers the ticket.
    let zip = pack.zip(app, dist, version)?;
    Ok(PackagedCut {
        dmg: dmg_out,
        dmg_sha256,
        dmg_size,
        zip,
    })
}

/// The remedy for verified bytes a claimed cut can no longer sign: never a rebuild.
fn spent_remedy(version: &str, build: u64) -> String {
    format!(
        "A claimed release is never rebuilt: build {build}'s number is spent, and nothing \
         rebuilt could be the bytes `verify release` judged. `{CUT_COMMAND} --abandon v{version}` \
         withdraws it; the next release reserves, builds and measures anew"
    )
}

/// Step "sign": the verified bundle, byte-copied out of the release cache and re-proved
/// → sign → notarize → DMG → provenance → manifest + notes, one re-enterable unit. The
/// cut builds nothing: the copy's digests are the record's ([`artifact::copy_bundle`]),
/// and the verified judge is asked again ([`gates::verified`]) before the first
/// signature. Bytes that are gone or changed are refused, never rebuilt.
fn step_sign(ctx: &mut CutCtx) -> Result<()> {
    if let Some(handoff) = &mut ctx.linux {
        handoff.run_workers(&ctx.version, ctx.build, &ctx.commit)?;
        handoff.import(&ctx.dist, &ctx.version, ctx.build, &ctx.commit)?;
        // Freeze before any signing or upload. A resume cannot exchange one
        // worker's bytes or provenance for another under an old proof.
        if let Some(journal) = &mut ctx.journal {
            journal.linux = Some(handoff.clone());
            journal.save(&ctx.journal_path)?;
        }
    }
    let (Some(key), Some(macho_sha256)) = (ctx.artifact.clone(), ctx.macho_sha256.clone()) else {
        return Err(Error::new(
            "internal: this cut names no verified bundle to sign — a cut signs only the \
             bundle its release built once",
        ));
    };
    let store = artifact::store(&ctx.repo).map_err(Error::new)?;
    let complete = artifact::read(&store, &key)
        .map_err(|why| {
            Error::new(format!(
                "{why} — refusing to sign it. {}",
                spent_remedy(&ctx.version, ctx.build)
            ))
        })?
        .ok_or_else(|| {
            Error::new(format!(
                "the verified bundle {key} is gone from the release cache ({}). {}",
                store.display(),
                spent_remedy(&ctx.version, ctx.build)
            ))
        })?;
    let identity = artifact::Identity {
        version: ctx.version.clone(),
        build: ctx.build,
        commit: ctx.commit.clone(),
    };
    if !complete.is(&identity) || complete.macho_sha256 != macho_sha256 {
        return Err(Error::new(format!(
            "the release cache's {key} is build {} of v{} from {} (executable {}), not this \
             cut's build {} of v{} from {} (executable {macho_sha256}) — refusing to sign it",
            complete.build,
            complete.version,
            complete.commit,
            complete.macho_sha256,
            ctx.build,
            ctx.version,
            ctx.commit
        )));
    }
    // Asked again just before the first signature: the record names these exact bytes,
    // verified in the tree of the release commit's one parent.
    let published = rev_parse(&GitCli::new(&ctx.tree), &format!("{}^1", ctx.commit))?;
    let (yes, line) = gates::verified(&GitCli::new(&ctx.repo), &published, &macho_sha256)?;
    if !yes {
        return Err(Error::new(format!(
            "NOTHING SHIPS UNVERIFIED: {line} — refusing to sign build {}",
            ctx.build
        )));
    }
    step("gate", &line);
    let app = bundle::prepare_staging(&ctx.dist, ctx.build).map_err(Error::new)?;
    artifact::copy_bundle(&complete, &complete.app(&store), &app).map_err(Error::new)?;
    artifact::place_records(&complete.dir(&store), &ctx.dist, &ctx.version).map_err(Error::new)?;
    step(
        "bundle",
        &format!(
            "the VERIFIED bundle, byte for byte: {} → {} — executable sha256 {}… (archs [{}] · \
             {}) · Short={} CFBundleVersion={}",
            complete.app(&store).display(),
            app.display(),
            &macho_sha256[..12],
            complete.archs,
            complete.compiler_line,
            ctx.version,
            ctx.build
        ),
    );

    let spec = bundle::BundleSpec {
        repo_root: ctx.tree.clone(),
        out_dir: ctx.dist.clone(),
        short_version: ctx.version.clone(),
        build_number: ctx.build,
        bundle_id: bundle::BUNDLE_ID.to_string(),
        git_commit: ctx
            .commit
            .get(..12)
            .ok_or_else(|| Error::new("the release commit is not a full commit id"))?
            .to_string(),
        aterm_bin: app.join("Contents/MacOS/aterm"),
    };

    // Tier APPLE, resolved once at the entry point. Inactive (a fork's tier) means
    // `identity()` is None and every hook below is a no-op: the ad-hoc path.
    let sign_id = ctx.apple.identity();
    let signed_by = sign::sign_app(
        &app,
        &ctx.tree.join("apps/aterm-mac/aterm.entitlements"),
        sign_id,
    )?;
    step(
        "sign",
        &(if sign_id.is_some() {
            format!("Developer ID: {signed_by}")
        } else {
            "ad-hoc (pins::APPLE_TEAM_ID is empty — Tier APPLE inactive)".to_string()
        }),
    );

    // Notarize, package and re-hash as one ordered unit (`notarize_and_package`, proved
    // offline in tests/it/apple_tier.rs).
    if ctx.apple.identity().is_some() {
        step("notarize", &notarize_notice(ctx.kind));
    }
    let PackagedCut {
        dmg: dout,
        dmg_sha256: dmg_sha,
        dmg_size,
        zip: zout,
    } = notarize_and_package(
        &app,
        &ctx.dist,
        &ctx.version,
        &ctx.apple,
        &sign::RealAppleTools,
        &RealPackager,
    )?;
    // The DMG must clear the client's own download bound
    // (`RELEASE_ASSET_DOWNLOAD_BOUND`) before anything is hashed into a manifest, or
    // the release is one no client can download.
    validate_release_asset_download_size(dmg_size)?;
    // ...and the shape question the bound cannot answer: a seeded image is downloadable
    // and still the wrong artifact. Checked on the bytes just produced.
    validate_lean_dmg_size(&ctx.version, dmg_size)?;
    // The stable download twins are copied only here, from the final container bytes
    // (codesign and staple rewrites included), so each is byte-identical to the bytes
    // its in-process digest covers. `required_asset_names()` lists them, so `publish`
    // uploads them and refuses a head without them; they are the permanent download
    // names the README publishes.
    for (source, twin) in [
        (&dout.path, ctx.stable_dmg_path()),
        (&zout.path, ctx.stable_zip_path()),
    ] {
        fs::copy(source, &twin).map_err(|e| {
            Error::new(format!(
                "copy {} -> {}: {e}",
                source.display(),
                twin.display()
            ))
        })?;
    }
    // Provenance after signing: binary_sha256 must cover the signed bytes, and
    // pre_sign_sha256 names the verified ones they were signed from.
    let provenance_path = bundle::write_provenance(&spec, &app, &signed_by, &macho_sha256)?;
    if ctx.signature_required {
        // Bind the provenance to the fingerprint the binary carries (the committed paper
        // master `aterm-gui/build.rs` embeds), never the signing key's: the field records
        // which anchor reached the artifact.
        let fingerprint = ctx
            .expected_embedded_pin()?
            .ok_or_else(|| Error::new("signed build has no pinned paper master"))?;
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
    // `.sha256` sidecars (`shasum -a 256 -c` records) for the containers and their
    // stable twins, from the same in-process digests that feed the manifest; the
    // release notes' verify instruction points at them. A `shasum -c` record names the
    // file it checks, so each twin's sidecar restates its digest under the alias name.
    let sidecars = [
        (
            ctx.dmg_sha256_path(),
            dmg_sha.as_str(),
            channel::dmg_asset_name(&ctx.version),
        ),
        (
            ctx.zip_sha256_path(),
            zout.sha256.as_str(),
            channel::zip_asset_name(&ctx.version),
        ),
        (
            ctx.stable_dmg_sha256_path(),
            dmg_sha.as_str(),
            channel::stable_dmg_asset_name(),
        ),
        (
            ctx.stable_zip_sha256_path(),
            zout.sha256.as_str(),
            channel::stable_zip_asset_name(),
        ),
    ];
    for (path, sha, name) in sidecars {
        fs::write(&path, channel::sha256_sidecar_contents(sha, &name))
            .map_err(|e| Error::new(format!("write {}: {e}", path.display())))?;
    }
    step(
        "",
        "`.sha256` sidecars staged for every container and every stable twin \
         (shasum -a 256 -c)",
    );
    // ---- manifest + notes (the rolled body, verbatim, once — spec §3) -----
    let cl_text = fs::read_to_string(ctx.tree.join(changelog::CHANGELOG_FILE))
        .map_err(|e| Error::new(format!("read {}: {e}", changelog::CHANGELOG_FILE)))?;
    let body = changelog::rolled_body(&cl_text, &ctx.notes_section)?;
    // The GitHub body gets the standing newcomer preamble; the manifest's
    // `changelog` below stays the rolled section verbatim — the in-app notes
    // address a machine that already runs aterm.
    fs::write(
        ctx.notes_path(),
        changelog::release_notes_document(&ctx.version, &body),
    )
    .map_err(|e| Error::new(format!("write {}: {e}", ctx.notes_path().display())))?;

    let plist_text = fs::read_to_string(app.join("Contents/Info.plist"))
        .map_err(|e| Error::new(format!("read stamped Info.plist: {e}")))?;
    let min_os = manifest_out::plist_string(&plist_text, "LSMinimumSystemVersion")
        .unwrap_or_else(|| "11.0".to_string());
    let inputs = manifest_out::ManifestInputs {
        version: &ctx.version,
        build_number: ctx.build,
        commit: &ctx.commit,
        dmg_name: &channel::dmg_asset_name(&ctx.version),
        dmg_sha256: &dmg_sha,
        zip_name: &channel::zip_asset_name(&ctx.version),
        // No re-hash: `create_zip` runs after the staple and hashes the bytes it writes.
        zip_sha256: &zout.sha256,
        // The manifest's `url` names the one release it rides: the channel this cut
        // publishes onto, which the
        // client's URL bind and the cut's own pointer gate both require.
        repo_slug: &ctx.slug,
        min_os: &min_os,
        team_id: aterm_update_core::pins::APPLE_TEAM_ID,
        pub_date: &bundle::epoch_to_rfc3339(unix_now()),
        min_build: ctx.min_build,
        changelog: &body,
    };
    let mpath = stage_manifest(
        &ctx.dist,
        &inputs,
        ctx.attribution.as_ref(),
        ctx.linux.as_ref(),
    )?;
    // Staged from the bytes the pre-claim gate authorized, and before the signature only
    // so a failure lands before a signature exists (the appcast signature does not
    // cover them).
    stage_roster_assets(&ctx.dist, ctx.roster.as_ref())?;
    // A re-entered `sign` may reuse dist/. Never let an earlier signed cut's
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
        // `run_pipeline` marks `sign` immediately after this returns; that
        // same atomic save persists the signature fact before later steps.
        journal.manifest_signed = ctx.manifest_signed;
        journal.signature_required = ctx.signature_required;
        journal.signature_pubkey.clone_from(&ctx.signature_pubkey);
        journal.verify_pubkey.clone_from(&ctx.verify_pubkey);
        journal
            .signature_machine_id
            .clone_from(&ctx.signature_machine_id);
    }
    Ok(())
}
/// The signing half of the self-check: the hard `codesign` gate every cut faces, and
/// the Tier APPLE evidence a cut faces iff its manifest claims a team; returns the
/// transcript suffix, so the transcript's claim is this verdict's own words. Gated on
/// the manifest's `team_id`, not `ctx.apple`: the manifest is the promise that ships.
/// Every spawn goes through [`sign::AppleTools`] (absolute tool paths, never `$PATH`),
/// which also makes the branch testable with no certificate.
pub fn selfcheck_signing(
    team: &str,
    app: &Path,
    dmg: &Path,
    tools: &dyn sign::AppleTools,
) -> Result<&'static str> {
    // The hard gate (sign.rs's inline verify print is advisory), on every tier,
    // including the ad-hoc one a fork ships.
    tools.codesign_verify_strict(app).map_err(|e| {
        Error::new(format!(
            "self-check failed: codesign --verify --deep --strict: {e}"
        ))
    })?;
    if team.is_empty() {
        // A cut that claims no team (a fork's tier) has no notarization promise to
        // keep: no Apple tool is spawned.
        return Ok("");
    }
    // Evidence gathered here, verdict passed in sign.rs, so the rules are testable
    // without an Apple account.
    sign::apple_selfcheck_verdict(&sign::AppleSelfcheck {
        team_id: team,
        app_codesign_dv: &tools.codesign_dv(app).map_err(Error::new)?,
        app_stapled: tools.stapler_validate(app).map_err(Error::new)?,
        app_gatekeeper_ok: tools
            .gatekeeper_ok(app, sign::GatekeeperKind::App)
            .map_err(Error::new)?,
        dmg_stapled: tools.stapler_validate(dmg).map_err(Error::new)?,
        dmg_gatekeeper_ok: tools
            .gatekeeper_ok(dmg, sign::GatekeeperKind::Dmg)
            .map_err(Error::new)?,
    })
    .map_err(Error::new)?;
    Ok(" · Tier APPLE: TeamIdentifier + stapled ticket + Gatekeeper on .app and .dmg")
}

/// The bytes on disk are the bytes the signed manifest names, re-proved by digest and
/// running nothing, because `dist/` is mutable and a resume skips `sign`.
/// [`step_selfcheck`] runs this; `publish` runs [`prove_publication_on_disk`] alone (a
/// recovered cut has no bundle). Returns the parsed on-disk manifest.
fn prove_artifacts_on_disk(ctx: &CutCtx) -> Result<Manifest> {
    prove_bundle_on_disk(ctx)?;
    prove_publication_on_disk(ctx)
}

/// The staged bundle's sealed identity: `CFBundleVersion` is the claimed build and
/// `CFBundleShortVersionString` the claimed version.
fn prove_bundle_on_disk(ctx: &CutCtx) -> Result<()> {
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
    Ok(())
}

/// Everything the release will carry, re-proved from `dist/` by digest: the staged
/// native Linux handoff (when the cut carries one), the lean DMG ceiling, the
/// provenance record, the manifest's identity and signature under the key the
/// published bytes verify under ([`CutCtx::verification_pubkey`], a dead publisher's
/// own on a cross-machine recovery), the DMG and zip against the manifest's digests,
/// the evergreen twins and the `.sha256` sidecars.
fn prove_publication_on_disk(ctx: &CutCtx) -> Result<Manifest> {
    if let Some(handoff) = &ctx.linux {
        handoff.verify_staged(&ctx.dist, &ctx.version, ctx.build, &ctx.commit)?;
    }
    // A resume may skip `step_sign` and dist/ is mutable, so re-read the artifact
    // before any publication-facing step trusts it. A fresh cut restages dist/, so
    // abandoning this one is the whole remedy.
    validate_lean_dmg_on_disk(
        &ctx.dmg_path(),
        "self-check",
        Some(&format!(
            "`{CUT_COMMAND} --abandon v{}` and cut again",
            ctx.version
        )),
    )?;

    let provenance = fs::read(ctx.provenance_path())
        .map_err(|error| Error::new(format!("read release provenance: {error}")))?;
    validate_claim_provenance(&provenance, &ctx.version, ctx.build, &ctx.commit)?;

    if ctx.signature_required {
        // The provenance records the pin the build expected, through the accessor
        // `step_sign` used. The binary's own report of it is read in `step_selfcheck`.
        let fingerprint = ctx
            .expected_embedded_pin()?
            .ok_or_else(|| Error::new("signed channel has no pinned paper master"))?;
        let provenance = fs::read_to_string(ctx.provenance_path())
            .map_err(|error| Error::new(format!("read update-pin provenance: {error}")))?;
        let expected = format!("update_pubkey_fingerprint_sha256={fingerprint}");
        if !provenance.lines().any(|line| line == expected) {
            return Err(Error::new(format!(
                "release provenance is missing exact update-pin field {expected:?}"
            )));
        }
    }

    // The manifest on disk (what will be uploaded), parsed with the shared type;
    // `manifest_out` proved the frozen fleet parser accepts it at emission.
    let mtext = fs::read_to_string(ctx.manifest_path())
        .map_err(|e| Error::new(format!("read {}: {e}", ctx.manifest_path().display())))?;
    let manifest = Manifest::parse(&mtext)
        .map_err(|e| Error::new(format!("self-check: manifest re-parse failed: {e}")))?;
    buildplan::linux::verify_manifest_handoff(&manifest, ctx.linux.as_ref())?;
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
            ctx.verification_pubkey().ok_or_else(|| {
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
    let zip_name = channel::zip_asset_name(&ctx.version);
    let zip_sha256 = match (manifest.zip.as_deref(), manifest.zip_sha256.as_deref()) {
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
            expected.to_string()
        }
        _ => {
            return Err(Error::new(
                "self-check failed: manifest carries no zip name + digest pair; the in-app \
                 updater cannot stage without `hdiutil`, which an orphaned post-handoff \
                 process cannot use",
            ));
        }
    };

    // The manifest names one DMG, proved above; a manifest that still names a
    // retired Intel variant was not staged by this cutter.
    refuse_retired_intel_dmg(&manifest)?;

    // The stable twins are re-proved too: `publish` verifies the channel's alias
    // objects against dist/'s twins, not the canonical containers, so a stale twin
    // would cross-verify against itself. A twin is only ever a byte copy, so hashing it
    // against the record's digest is the identity proof. A missing twin is regenerated
    // from the proven canonical bytes; a present one with other bytes is refused, not
    // repaired: something else wrote it.
    for (twin, source, expected_sha, what) in [
        (
            ctx.stable_dmg_path(),
            ctx.dmg_path(),
            manifest.sha256.as_str(),
            "stable DMG twin",
        ),
        (
            ctx.stable_zip_path(),
            ctx.zip_path(),
            zip_sha256.as_str(),
            "stable zip twin",
        ),
    ] {
        if !twin.exists() {
            fs::copy(&source, &twin).map_err(|e| {
                Error::new(format!(
                    "self-check: copy {} -> {}: {e}",
                    source.display(),
                    twin.display()
                ))
            })?;
        }
        let sha = dmg::sha256_file(&twin)?;
        if !sha.eq_ignore_ascii_case(expected_sha) {
            return Err(Error::new(format!(
                "self-check failed: {what} {} sha256 {sha} != manifest {expected_sha} — \
                 the evergreen `releases/latest/download` alias would serve different \
                 bytes than the release it fronts",
                twin.display()
            )));
        }
    }

    // The `.sha256` sidecars on disk must state exactly the manifest's digests: a stale
    // one would ship a record that fails against the bytes beside it. Byte equality
    // against the regenerated record (one legal spelling, `<hash>  <name>\n`). A
    // missing sidecar is regenerated: sidecars are pure functions of digests the
    // manifest binds, as in `recover_published_cut`. The twins' alias sidecars follow
    // the same rule under their alias filenames.
    let stable_dmg_name = channel::stable_dmg_asset_name();
    let stable_zip_name = channel::stable_zip_asset_name();
    let sidecar_checks = [
        (
            ctx.dmg_sha256_path(),
            manifest.sha256.as_str(),
            manifest.dmg.as_str(),
        ),
        (
            ctx.zip_sha256_path(),
            zip_sha256.as_str(),
            zip_name.as_str(),
        ),
        (
            ctx.stable_dmg_sha256_path(),
            manifest.sha256.as_str(),
            stable_dmg_name.as_str(),
        ),
        (
            ctx.stable_zip_sha256_path(),
            zip_sha256.as_str(),
            stable_zip_name.as_str(),
        ),
    ];
    for (path, sha, name) in sidecar_checks {
        let expected = channel::sha256_sidecar_contents(sha, name);
        if !path.exists() {
            fs::write(&path, &expected)
                .map_err(|e| Error::new(format!("self-check: write {}: {e}", path.display())))?;
        }
        let observed = fs::read_to_string(&path)
            .map_err(|e| Error::new(format!("self-check: read {}: {e}", path.display())))?;
        if observed != expected {
            return Err(Error::new(format!(
                "self-check failed: {} does not carry the manifest's digest record \
                 (expected {expected:?}, found {observed:?}) — a stale sidecar would \
                 fail `shasum -c` against the artifact beside it",
                path.display()
            )));
        }
    }
    Ok(manifest)
}

/// Step "selfcheck" (spec §7 step 4): the artifacts are the manifest's
/// ([`prove_artifacts_on_disk`]), then the cut's one behavioural pass over the signed
/// bytes (binary == plist == manifest == n, the argv0 identities, a headless session
/// the signed native slice drives, `codesign`, Tier APPLE) and the monotonic check. No
/// paint smoke: `verify release` measured the executable this copy carries; what
/// signing adds (hardened runtime, entitlements, seal) is what runs here.
fn step_selfcheck(ctx: &mut CutCtx) -> Result<()> {
    let manifest = prove_artifacts_on_disk(ctx)?;
    let app = ctx.app_path();

    // Binary stamp == n. The GUI binary prints no raw build number, but `--diagnose`
    // prints ATERM_BUILD_TIME, which build.rs derives bijectively from
    // SOURCE_DATE_EPOCH, i.e. from n.
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

    if ctx.signature_required {
        // The shipped binary embedded the pin the build expected, through the accessor
        // `step_sign` used (the provenance half is `prove_artifacts_on_disk`'s).
        let fingerprint = ctx
            .expected_embedded_pin()?
            .ok_or_else(|| Error::new("signed channel has no pinned paper master"))?;
        buildplan::validate_slice_update_pin_reports(
            &fingerprint,
            &[("shipped universal", &diag_text)],
        )?;
        step(
            "",
            &format!(
                "binary runtime reports pinned update key {}…; per-slice/provenance proof bound",
                &fingerprint[..12]
            ),
        );
    }

    // The signed native slice, launched headless and hermetic, answers one typed turn
    // and draws its first frame on the GPU renderer, which the hardened runtime,
    // library validation or the signed entitlements would stop. Its waits are hang
    // detectors.
    buildplan::signed_slice_drives_a_session(&app).map_err(|why| {
        Error::new(format!(
            "self-check failed: the SIGNED bundle does not run a session on the GPU ({why})"
        ))
    })?;
    step(
        "session",
        "the signed arm64 slice drove a headless session (`uname -m` answered arm64) and \
         drew its first frame on the GPU renderer (`ctl metrics` backend=gpu)",
    );
    // codesign + Tier APPLE (spec §7 step 4, the tier iff the manifest claims a
    // team), after the signed bytes ran and before any publish-facing step. The
    // suffix is the verdict's own words — see `selfcheck_signing`.
    let team = manifest.team_id.clone().unwrap_or_default();
    let apple_note = selfcheck_signing(&team, &app, &ctx.dmg_path(), &sign::RealAppleTools)?;
    step(
        "selfcheck",
        &format!(
            "binary == plist == manifest == {} · codesign --verify --deep --strict ok{apple_note}",
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

/// The `roster_seq` a published channel head was cut under, parsed from the manifest
/// text `verify::Published` already carries. `None`: the head carries no attribution
/// (an unarmed channel).
fn published_roster_seq(newest: Option<&verify::Published>) -> Result<Option<u64>> {
    let Some(published) = newest else {
        return Ok(None);
    };
    Ok(Manifest::parse(&published.text)
        .map_err(|e| {
            Error::new(format!(
                "channel head {} carries a manifest this cutter cannot parse ({e}); \
                 refusing to reason about its machine roster generation",
                published.tag
            ))
        })?
        .roster_seq)
}

/// The `roster_seq` this cut will publish: before `sign`, the pre-claim attribution;
/// after, the staged manifest (the bytes that ship; a resume past `sign` carries no
/// attribution). A cut that attaches a roster but can answer neither is an error,
/// since `None` reads as "no roster in play" to [`roster_floor_covered`].
fn cut_roster_seq(ctx: &CutCtx) -> Result<Option<u64>> {
    if let Some(who) = &ctx.attribution {
        return Ok(Some(who.roster_seq));
    }
    if !ctx.attaches_roster() {
        return Ok(None);
    }
    let text = fs::read_to_string(ctx.manifest_path()).map_err(|e| {
        Error::new(format!(
            "read {} to learn which machine-roster generation this cut carries: {e}",
            ctx.manifest_path().display()
        ))
    })?;
    Ok(Manifest::parse(&text)
        .map_err(|e| Error::new(format!("staged manifest re-parse failed: {e}")))?
        .roster_seq)
}

/// Replay the client selection against the publish target and apply both the
/// monotonic-build gate and the carried-floor gate; returns the selected live
/// build for the transcript.
fn best_published(ctx: &CutCtx) -> Result<Option<u64>> {
    let scanned = verify::scan_published(&ctx.slug, true)?;
    let best = scanned.first();
    let newest_floor = best.and_then(|published| published.min_build);
    // The roster ratchet rides with the `min_build` ratchet, at every place that
    // guards it — lock, selfcheck, and both ratchets of `publish` — because it closes
    // the same race for the same reason: another publisher's release can land between
    // this cut's pre-claim scan and its head PATCH, and a channel head is never allowed
    // to become visible under a roster generation the fleet has already moved past.
    //
    // BOTH numbers, as at pre-claim: the head MANIFEST's attribution and the roster
    // ASSET the public channel actually serves. A machine joining the roster attaches
    // the new pair to already-published releases WITHOUT re-signing their manifests,
    // and every client ratchets on the asset it observed — so a join that lands
    // between this cut's pre-claim and its flip moves the fleet's floor while the
    // manifest number stays put. Reading only the manifest would let such a cut go
    // live under the old generation and strand every client that had ratcheted.
    // A channel that cannot be read fails closed: a wrong answer
    // here burns a build number and strands the fleet.
    let manifest_roster_seq = published_roster_seq(best)?;
    let observed_roster = if ctx.kind == CutKind::Real {
        machines::channel_roster_document(&ctx.slug).map_err(|e| {
            Error::new(format!(
                "cannot read the machine roster on the channel {}'s latest release ({e}); \
                 refusing to reason about the fleet's roster floor",
                ctx.slug
            ))
        })?
    } else {
        None
    };
    let observed_roster_seq = observed_roster.as_ref().map(|(seq, _)| *seq);
    let carried = cut_roster_seq(ctx)?;
    if let Ok(local_roster) = fs::read(ctx.dist.join(roster::ROSTER_ASSET)) {
        machines::roster_lineage_agrees(&local_roster, carried, observed_roster.as_ref())
            .map_err(Error::new)?;
    }
    let newest_roster_seq = match (manifest_roster_seq, observed_roster_seq) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    roster_floor_covered(carried, newest_roster_seq)?;
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

/// Step "tag" (real cut only): the annotated `vX.Y.Z` tag on origin, naming the
/// release commit — pushed after the self-check and before anything is published,
/// so the release `publish` makes the channel head always names a tag origin
/// carries. It stays on the dev repository (the channel's own tag is the engine's);
/// a cut abandoned after this step deletes it (`--abandon`), and a lost machine's
/// recovery does too.
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
        // at our commit; anything else would re-point a name we're publishing.
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

// ---------------------------------------------------------------------------
// step "publish": the one publication, straight onto the release channel
// ---------------------------------------------------------------------------

/// Proof that the release channel is reachable, public and writable by this operator's
/// credential: pre-claim, and again inside `publish` before the first write. A missing
/// push permission found at `publish` would burn a build number and hold the lease
/// until an owner-level grant no resume can supply.
///
/// `claimed` decides only the refusal's words: whether a build number was claimed yet,
/// and so what comes after the fix.
pub fn preflight_channel_target(slug: &str, claimed: bool) -> Result<()> {
    let endpoint = format!("repos/{slug}");
    let out = gh_retry(&[
        "api",
        &endpoint,
        "--jq",
        r#"[(.private | tostring), (.permissions.push // false | tostring)] | @tsv"#,
    ])
    .map_err(|error| {
        Error::new(format!(
            "cannot read the release channel {slug}: {error}. A 404 here means the \
             repository does not exist or this credential cannot see it; create it (public) \
             and grant the release account write access."
        ))
    })?;
    let row = out.stdout_utf8();
    let row = row.trim();
    let mut fields = row.split('\t');
    let (Some(private), Some(push), None) = (fields.next(), fields.next(), fields.next()) else {
        return Err(Error::new(format!(
            "release channel {slug} returned a malformed repository row {row:?}"
        )));
    };
    let then = if claimed {
        format!(", then `{CUT_COMMAND}`")
    } else {
        "; nothing was claimed".to_string()
    };
    if push != "true" {
        let token_path = channel_token_path().map_or_else(
            || "~/.secrets/gh_access_token_alabsystems".to_string(),
            |path| path.display().to_string(),
        );
        // With no token loaded, `gh` ran as the plain `gh auth` account
        // ([`active_channel_token`]): the token was never asked.
        return Err(Error::new(if channel_token().is_some() {
            format!(
                "the release token ({token_path}) cannot write {slug}: grant it write access{then}"
            )
        } else {
            format!(
                "no release token in {token_path}, and gh auth cannot write {slug}: put the \
                 release-org token there{then}"
            )
        }));
    }
    if private == "true" {
        return Err(Error::new(format!(
            "{slug} is private, so no install can read it: make it public{then}"
        )));
    }
    Ok(())
}

/// Validate a channel release object capability: the immutable release ID, the tag
/// and the draft state — the three things a mutation must not have raced. There is
/// no `target_commitish` to bind: the channel is not the repository the claim commit
/// lives in, so its releases are anchored at the channel's default branch, and the
/// bytes' authenticity comes from the manifest digest, the pinned signature and
/// codesign, never from a release target.
fn validate_channel_release_capability(
    observed: Option<&ReleaseObjectIdentity>,
    expected_id: u64,
    expected_tag: &str,
    expected_draft: bool,
) -> Result<()> {
    let observed = observed.ok_or_else(|| {
        Error::new(format!(
            "channel release ID {expected_id} vanished before mutation"
        ))
    })?;
    if observed.id != expected_id
        || observed.tag != expected_tag
        || observed.draft != expected_draft
    {
        return Err(Error::new(format!(
            "channel release ID {expected_id} changed tag/state; refusing mutation"
        )));
    }
    Ok(())
}

/// Step "publish": the one publication, and the step that makes auto-update work.
/// Runs [`channel::publish_on_channel`] (which documents the sequence: floors, bind,
/// uploads by immutable ID, asset proof, floors again, the one guarded head PATCH, the
/// read side) over this cut's release, with `dist/` re-proved against the signed appcast
/// ([`prove_publication_on_disk`]) and the roster pair against the master signature
/// ([`prove_roster_on_disk`]).
fn step_publish(ctx: &mut CutCtx) -> Result<()> {
    ensure_ctx_release_lease(ctx)?;
    let slug = ctx.slug.clone();
    // Every call below talks to the channel and nothing else — the asset bytes come
    // from `dist/` — under the release-org credential `run_pipeline` holds for a real
    // cut.
    preflight_channel_target(&slug, true)?;
    channel::publish_on_channel(&mut LiveChannelRelease {
        ctx,
        slug: &slug,
        bound: None,
    })?;
    step(
        "publish",
        &format!("v{} (build {}) published on {slug}", ctx.version, ctx.build),
    );
    Ok(())
}

/// The roster the signed appcast names. The pair in `dist/` is the one file an
/// un-lease-gated ceremony (`atpkg-keys join`, `ship provision`) may rewrite between
/// `sign` and a resumed `publish`, and an unchecked upload could publish a roster the
/// appcast's attribution contradicts, which every armed client refuses with no
/// fallback. So it is held to the client's rule ([`machines::verify_published_roster`]):
/// master-signed, naming the appcast's `machine_id`, at its `roster_seq` or newer (a
/// join's newer generation is what the fleet admits).
fn prove_roster_on_disk(ctx: &CutCtx, manifest: &Manifest) -> Result<()> {
    if !ctx.attaches_roster() {
        return Ok(());
    }
    let machine_id = manifest.machine_id.as_deref().ok_or_else(|| {
        Error::new("this cut attaches a machine roster but its appcast names no machine_id")
    })?;
    if Some(machine_id) != ctx.signature_machine_id.as_deref() {
        return Err(Error::new(format!(
            "the appcast is attributed to machine {machine_id:?}, but this cut's journal \
             records {:?}; refusing to publish an attribution its own journal contradicts",
            ctx.signature_machine_id
        )));
    }
    let read = |name: &str| {
        fs::read(ctx.dist.join(name))
            .map_err(|e| Error::new(format!("read dist/{name} before publishing it: {e}")))
    };
    machines::verify_published_roster(
        aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        read(roster::ROSTER_ASSET)?,
        &read(roster::ROSTER_SIG_ASSET)?,
        machine_id,
        manifest.roster_seq,
    )
    .map_err(|e| {
        Error::new(format!(
            "dist/{} is not a roster the signed appcast names ({e}) — a join or provision \
             rewrote it after this cut was built. Put back the pair it was built under, then \
             `{CUT_COMMAND}`; or `{CUT_COMMAND} --abandon v{}` and cut again",
            roster::ROSTER_ASSET,
            ctx.version
        ))
    })?;
    Ok(())
}

/// Bind this cut's release on the channel `slug` ([`channel::ChannelRelease::bind`]):
/// the journal's durable intent and the release's immutable ID, re-read and
/// capability-checked. Returns the ID and whether the release was already visible
/// (adopted) when bound.
fn bind_channel_release(ctx: &mut CutCtx, slug: &str) -> Result<(u64, bool)> {
    let observed = unique_release_object_by_tag(slug, &ctx.tag)?;
    // `adopted`: the release was already visible (not a draft) when this step bound
    // it — the source release `pub publish` created, or this cut's own adoption from a
    // previous pass. (A release this cut already made the head never reaches the bind:
    // `head_made` finishes it first.)
    let mut adopted = false;
    let release_id = match channel::bind_plan(
        ctx.release_intent,
        observed.as_ref().map(|release| release.draft),
    ) {
        channel::BindPlan::AwaitVisibility => {
            return Err(Error::new(format!(
                "the release intent for {} on {slug} was already durably issued, but no \
                 release object is visible; refusing a duplicate POST. Re-run \
                 `{CUT_COMMAND}` after GitHub converges.",
                ctx.tag
            )));
        }
        channel::BindPlan::CreateDraft => {
            let release = create_channel_draft(ctx, slug)?;
            step("publish", &format!("draft {} created on {slug}", ctx.tag));
            release.id
        }
        channel::BindPlan::ConvergeDraft => {
            let release = observed.expect("visible draft decision");
            // A draft this cut never issued a create POST for is not ours to adopt: the
            // journal refuses an object ID without its durable intent, so say why here
            // rather than fail opaquely in a journal save.
            if !ctx.release_intent {
                return Err(Error::new(format!(
                    "a draft release for {} already exists on {slug} (ID {}) but this cut \
                     never issued a create POST for it — it is a leftover or foreign object, \
                     and adopting it would bind a capability with no durable intent. Inspect \
                     and delete it, then `{CUT_COMMAND}`.",
                    ctx.tag, release.id
                )));
            }
            step(
                "publish",
                &format!(
                    "draft {} ID {} already on {slug} — converging",
                    ctx.tag, release.id
                ),
            );
            release.id
        }
        channel::BindPlan::ConvergeVisible => {
            // Two benign readings, both converging through the same sequence: this
            // cut's own durable adoption from a previous pass, or the source release
            // `pub publish` created first (a prerelease, so it never holds `latest`),
            // carrying only its attestation and roster pairs. Anything else under our
            // tag is another cut's.
            let release = observed.expect("visible release decision");
            if ctx.release_intent {
                // Our durable adoption from a previous pass: a crash between intent and
                // completion resumes here with a partial asset set, which is ours.
                step(
                    "publish",
                    &format!("{} on {slug} — resuming this cut's own release", ctx.tag),
                );
            } else {
                adoptable_channel_release(ctx, slug, release.id)?;
                // An ID implies our durable intent, so the adoption is made durable
                // first (the permit is unused: nothing will POST, the visible release is
                // the object), and only then is the ID bound. A crash after this resumes
                // into the arm above.
                let _adoption = ctx.persist_release_intent()?;
                step(
                    "publish",
                    &format!(
                        "{} on {slug} is the SOURCE release (attestation pair, no binaries) \
                         — putting this cut's assets on it",
                        ctx.tag
                    ),
                );
            }
            adopted = true;
            release.id
        }
    };
    ctx.bind_release_id(release_id)?;
    let reread = release_object_by_id(slug, release_id)?;
    validate_channel_release_capability(reread.as_ref(), release_id, &ctx.tag, !adopted)?;
    Ok((release_id, adopted))
}

/// May this cut adopt the visible release under its tag that its journal holds no
/// intent for? Only as the engine's source release
/// ([`channel::unclaimed_is_source_release`]), decided from its asset listing, and only
/// while it is still a prerelease ([`channel::unclaimed_source_is_prerelease`]): the
/// cutter checks that itself, because the prerelease flag is what keeps this cut's
/// uploads, the appcast included, from any client until the head PATCH.
///
/// # Errors
/// Another cut's release, naming it and why; a full source release; and any listing
/// failure.
fn adoptable_channel_release(ctx: &CutCtx, slug: &str, release_id: u64) -> Result<()> {
    let names: Vec<String> = release_asset_inventory_for_release_id(slug, release_id)?
        .into_iter()
        .map(|asset| asset.name)
        .collect();
    channel::unclaimed_is_source_release(&names).map_err(|why| {
        Error::new(format!(
            "{} on {slug} (release ID {release_id}) {why}, and this cut holds no intent for \
             it: it is another cut's release, and adopting it would publish someone else's \
             bytes as this cut — withdraw it with that cut's `--abandon` (or `recover`), or \
             inspect it by hand.",
            ctx.tag
        ))
    })?;
    channel::unclaimed_source_is_prerelease(release_prerelease_by_id(slug, release_id)?).map_err(
        |why| {
            Error::new(format!(
                "{tag} on {slug} (release ID {release_id}) {why}. Refusing to adopt it: every \
                 upload would land on the channel's live head, where a client can elect the \
                 appcast before the asset proof and the second floor read, and `--abandon` \
                 could no longer withdraw it. Nothing was bound or uploaded. Mark it a \
                 prerelease (`gh release edit {tag} --prerelease --repo {slug}`), confirm the \
                 evergreen pointer names the previous app release again, fix the engine side \
                 so the next source release is created as one, then `{CUT_COMMAND}`.",
                tag = ctx.tag
            ))
        },
    )
}

/// The `prerelease` flag of release `id` on `slug`, from one exact-ID read.
fn release_prerelease_by_id(slug: &str, id: u64) -> Result<bool> {
    let endpoint = format!("repos/{slug}/releases/{id}");
    let out = gh_retry(&["api", &endpoint, "--jq", ".prerelease | tostring"])?;
    parse_prerelease_flag(&out.stdout_utf8())
        .map_err(|why| Error::new(format!("release ID {id} on {slug}: {why}")))
}

/// `"true"` / `"false"` (one trailing newline allowed), and nothing else: an absent or
/// garbled flag (`null`, an empty read) is an error, never an answer either way.
pub(crate) fn parse_prerelease_flag(text: &str) -> std::result::Result<bool, String> {
    match text.strip_suffix('\n').unwrap_or(text) {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!("invalid prerelease flag {other:?}")),
    }
}

/// [`channel::ChannelRelease`] against GitHub: this cut's bound release on the channel.
struct LiveChannelRelease<'a> {
    ctx: &'a mut CutCtx,
    slug: &'a str,
    /// Set by [`channel::ChannelRelease::bind`]: the release's immutable ID, and whether
    /// it was already visible (adopted) when bound, in which case every capability
    /// check before the head PATCH expects `draft == false`.
    bound: Option<(u64, bool)>,
}

impl LiveChannelRelease<'_> {
    /// The bound release's ID and whether it was adopted.
    fn bound(&self) -> Result<(u64, bool)> {
        self.bound.ok_or_else(|| {
            Error::new(
                "the channel sequence reached a release call before binding a release; \
                 refusing (channel::publish_on_channel binds right after the floors)",
            )
        })
    }
}

impl channel::ChannelRelease for LiveChannelRelease<'_> {
    fn head_made(&mut self) -> Result<bool> {
        // The head PATCH goes only to a bound release, and the bind journals its intent
        // and its ID before any write: a journal holding neither has made no head.
        let Some(release_id) = self.ctx.release_id.filter(|_| self.ctx.release_intent) else {
            return Ok(false);
        };
        let (slug, tag) = (self.slug, self.ctx.tag.as_str());
        // The predicate `--abandon` refuses on (`verify::run_abandon`), so every state
        // abandon will not touch, the resume finishes.
        if verify::release_state(slug, tag)? != verify::ReleaseState::Published {
            return Ok(false);
        }
        let live = release_object_by_id(slug, release_id)?;
        if !live
            .as_ref()
            .is_some_and(|live| live.id == release_id && live.tag == tag && !live.draft)
        {
            return Err(Error::new(format!(
                "{tag} is a published app release on {slug}, but not the release this \
                 journal bound (ID {release_id}: {live:?}) — inspect it by hand; nothing was \
                 written"
            )));
        }
        self.bound = Some((release_id, true));
        step(
            "publish",
            &format!(
                "{tag} ID {release_id} on {slug} is already the published app release (its \
                 head PATCH landed on an earlier pass): nothing to upload; re-asserting \
                 `latest` and proving what a stranger sees"
            ),
        );
        Ok(true)
    }

    fn artifacts(&mut self) -> Result<Vec<PathBuf>> {
        let ctx = &*self.ctx;
        let manifest = prove_publication_on_disk(ctx)?;
        prove_roster_on_disk(ctx, &manifest)?;
        let files = ctx.channel_asset_paths();
        if let Some(missing) = files.iter().find(|file| !file.is_file()) {
            return Err(Error::new(format!(
                "release asset missing: {} — this cut's dist/ artifacts are gone, so the \
                 channel cannot be served the bytes the self-check proved; \
                 `{CUT_COMMAND} --abandon v{}` and cut again",
                missing.display(),
                ctx.version
            )));
        }
        Ok(files)
    }

    fn upload(&mut self, file: &Path) -> Result<()> {
        let (release_id, adopted) = self.bound()?;
        upload_channel_asset(self.ctx, self.slug, release_id, file, !adopted)
    }

    fn prove_assets(&mut self) -> Result<()> {
        // From a fresh remote listing: exactly the asset set this cut publishes, each
        // object byte-identical to its `dist/` artifact, before the release can become
        // the head.
        let (release_id, adopted) = self.bound()?;
        prove_channel_assets(self.ctx, self.slug, release_id, !adopted)
    }

    fn ratchet_head(&mut self) -> Result<()> {
        let (slug, ctx) = (self.slug, &*self.ctx);
        // The pointer names this cut or an older release, or the cut does not take the
        // head (`prove_channel_head_is_older`).
        prove_channel_head_is_older(
            slug,
            &ctx.tag,
            ctx.build,
            &mut || {
                probe_evergreen_pointer(
                    slug,
                    manifest_out::MANIFEST_ASSET,
                    &aterm_update_core::pointer::canonical_app_tag,
                )
            },
            &mut anonymous_get_optional,
        )
    }

    fn ratchet(&mut self) -> Result<()> {
        self.ratchet_head()?;
        // The floors, read from the channel now: the carried apply floor, the monotonic
        // build, and the roster generation (the head manifest's attribution and the
        // roster asset the channel serves; a join is not lease-gated) with the
        // lineage-fork check (`best_published`). A resume can reach this step alone,
        // days after the self-check read them.
        best_published(self.ctx).map(|_| ())
    }

    fn bind(&mut self) -> Result<()> {
        self.bound = Some(bind_channel_release(self.ctx, self.slug)?);
        Ok(())
    }

    fn make_head(&mut self) -> Result<()> {
        let (release_id, adopted) = self.bound()?;
        let (slug, expect_draft) = (self.slug, !adopted);
        let ctx = &*self.ctx;
        let argv = channel::head_patch_argv(&format!("repos/{slug}/releases/{release_id}"));
        gh_retry_guarded(&argv.iter().map(String::as_str).collect::<Vec<_>>(), || {
            let current = release_object_by_id(slug, release_id)?;
            validate_channel_release_capability(
                current.as_ref(),
                release_id,
                &ctx.tag,
                expect_draft,
            )?;
            ensure_ctx_release_lease(ctx)?;
            Ok(())
        })?;
        step(
            "publish",
            &format!(
                "{} ID {release_id} on {slug} → full release, make_latest=true",
                ctx.tag
            ),
        );
        Ok(())
    }

    fn prove_head(&mut self) -> Result<()> {
        let (release_id, _) = self.bound()?;
        let slug = self.slug;
        let ctx = &*self.ctx;
        let after = release_object_by_id(slug, release_id)?;
        validate_channel_release_capability(after.as_ref(), release_id, &ctx.tag, false)?;
        prove_client_elects_this_cut(ctx, slug, release_id)?;
        // Everything above ran through `gh`, with a credential: it proves the release
        // exists, not that a machine with no credential can read it. A private or
        // membership-restricted channel passes every authenticated proof and is
        // invisible to every client. So the pointer gate, then one HEAD per asset, both
        // on the unmetered download host.
        prove_evergreen_pointer_serves_this_cut(ctx, slug)?;
        prove_channel_assets_download_anonymously(ctx, slug)
    }
}

/// How many times a post-flip anonymous probe asks before it reports failure. A draft
/// flipped live does not become anonymously readable atomically: the release object,
/// the asset listing and the download CDN converge within seconds of each other.
/// Retrying does not weaken the proof: a client arriving seconds after the flip is the
/// real case, and a genuinely incomplete upload fails every attempt.
///
/// A refusal (403 or 429, [`anon_probe_refusal`]) is not retried: it is not
/// convergence, and asking again only asks to be refused again while holding the lease.
const ANON_PROBE_ATTEMPTS: u32 = 10;

/// Gap between anonymous probe attempts.
const ANON_PROBE_DELAY: Duration = Duration::from_secs(6);

/// Attempts for the evergreen pointer to name this cut once its head PATCH is sent
/// ([`prove_pointer_serves`]). GitHub recomputes `releases/latest` behind a cache that
/// is slower than an asset's CDN (it has outlasted the whole anonymous budget,
/// 10 × 6 s, and moved a minute later): five minutes, and still a bound.
pub const POINTER_PROBE_ATTEMPTS: u32 = 30;

/// Gap between evergreen-pointer attempts.
pub const POINTER_PROBE_DELAY: Duration = Duration::from_secs(10);

/// The HTTP status of a refused anonymous probe (403 or 429), read from `curl -f`'s
/// stderr (exit 22 for every 4xx, the status named in the message, the shape the
/// client's `download_bytes` reads), or `None` for anything else. Not called a rate
/// limit: every probe of this step goes to `github.com/…/releases/…`, which sends no
/// `x-ratelimit-*` headers (`aterm_update_core::cdn`), so a 403 is a proxy, firewall or
/// abuse block and a 429 the web host's throttle, never the 60/hour API budget.
#[must_use]
pub fn anon_probe_refusal(stderr: &str) -> Option<u16> {
    let idx = stderr.find("returned error: ")?;
    let rest = &stderr[idx + "returned error: ".len()..];
    let code: String = rest.chars().take_while(char::is_ascii_digit).collect();
    match code.as_str() {
        "403" => Some(403),
        "429" => Some(429),
        _ => None,
    }
}

/// Run one anonymous `curl` probe, retrying (about a minute, to outlast GitHub's
/// eventual consistency after a flip) while it fails for any reason but a refusal
/// ([`anon_probe_refusal`], asked once). Credentials are stripped from the child on
/// every attempt, to see the channel as an install with no token does.
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
        if out.status.success()
            || anon_probe_refusal(&String::from_utf8_lossy(&out.stderr)).is_some()
        {
            return Ok(out);
        }
        last = Some(out);
        if attempt < ANON_PROBE_ATTEMPTS {
            std::thread::sleep(ANON_PROBE_DELAY);
        }
    }
    Ok(last.expect("at least one attempt"))
}

/// What one credential-free, redirect-refusing HEAD of a release asset's download URL
/// says about whether a stranger can download it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetHead {
    /// A 302 or 307 whose `Location` is an https URL on GitHub's release-asset
    /// storage (`atpkg::index_probe::is_release_asset_host`): the asset is published
    /// and served to anyone. The same classification atpkg's index probe applies to
    /// the same host.
    Downloads,
    /// 404: not served. Either the CDN has not converged after the flip (retried),
    /// or the channel repository is not public — GitHub renders a private
    /// repository's download URL as 404 on this host.
    NotServed,
    /// 403 or 429: refused. Asked once, never retried, and never read as a verdict
    /// about the channel — see [`anon_probe_refusal`].
    Refused(u16),
    /// Anything else, in the wire's words: a 200 (GitHub never serves an asset from
    /// this host itself, so a 200 is an intermediary's page — a captive portal
    /// answers every URL with one), a redirect anywhere but the asset storage, a
    /// transport failure. Retried like `NotServed`, because a network blip is weather.
    Inconclusive(String),
}

/// Classify one HEAD answer. Pure, so the whole table is a test.
#[must_use]
pub fn classify_asset_head(
    answer: std::result::Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError>,
) -> AssetHead {
    match answer {
        Ok(aterm_update_core::HeadAnswer {
            code: 302 | 307,
            location: Some(location),
        }) if atpkg::vendor::https_host(&location)
            .is_some_and(atpkg::index_probe::is_release_asset_host) =>
        {
            AssetHead::Downloads
        }
        Ok(aterm_update_core::HeadAnswer { code: 404, .. }) => AssetHead::NotServed,
        Ok(aterm_update_core::HeadAnswer {
            code: code @ (403 | 429),
            ..
        }) => AssetHead::Refused(code),
        Ok(aterm_update_core::HeadAnswer { code, location }) => {
            AssetHead::Inconclusive(match location {
                Some(location) => format!("HTTP {code} → {location}"),
                None => format!("HTTP {code}"),
            })
        }
        Err(error) => AssetHead::Inconclusive(error.to_string()),
    }
}

/// The readability proof: every asset the deployed updater elects downloads from the
/// public channel with no credential, by one redirect-refusing HEAD of each asset's
/// download URL (`https://github.com/<slug>/releases/download/<tag>/<name>`) and nothing
/// addressed to `api.github.com`. The anonymous API budget is 60 per hour per IP, shared
/// by every tool on the address, and an API read proves only that a release is listed;
/// the download host is unmetered (`aterm_update_core::cdn`) and answers per asset. The
/// HEAD carries no credential by construction (`head_no_redirect`), so an ambient
/// `GH_TOKEN` cannot fake readability. Fails closed. Returns the number of HEADs issued.
pub fn prove_assets_download_anonymously(
    slug: &str,
    tag: &str,
    names: &[String],
    head: &mut dyn FnMut(
        &str,
    ) -> std::result::Result<
        aterm_update_core::HeadAnswer,
        aterm_update_core::HttpError,
    >,
    sleep: &mut dyn FnMut(Duration),
) -> Result<u32> {
    let (owner, repo) = slug
        .split_once('/')
        .ok_or_else(|| Error::new(format!("not an owner/repo slug: {slug}")))?;
    let mut issued = 0_u32;
    for name in names {
        let url = aterm_update_core::cdn::release_download_url(owner, repo, tag, name).ok_or_else(
            || {
                Error::new(format!(
                    "no download URL can be derived for {name:?} of {tag} in {slug}"
                ))
            },
        )?;
        // Structural: the builder only produces `github.com` URLs, and this guard is
        // what keeps the proof off the anonymous API budget.
        if aterm_update_core::cdn::is_api_host(&url) {
            return Err(Error::new(format!(
                "refusing to probe {url}: the readability proof never spends the metered \
                 anonymous API"
            )));
        }
        let mut last = AssetHead::NotServed;
        for attempt in 1..=ANON_PROBE_ATTEMPTS {
            issued += 1;
            last = classify_asset_head(head(&url));
            match &last {
                AssetHead::Downloads => break,
                AssetHead::Refused(code) => {
                    return Err(Error::new(format!(
                        "GitHub's download host answered HTTP {code} to an unauthenticated \
                         HEAD of {url} — not the API budget (that host has none): a 403 is a \
                         proxy, a firewall or an abuse block, a 429 is GitHub throttling this \
                         address. Asked once; it says nothing about whether {slug} is public. \
                         Check from another network (`curl -sI {url}` answers 302 when the \
                         asset is served), then `{CUT_COMMAND}` (re-uploads nothing)."
                    )));
                }
                AssetHead::NotServed | AssetHead::Inconclusive(_) => {
                    if attempt < ANON_PROBE_ATTEMPTS {
                        sleep(ANON_PROBE_DELAY);
                    }
                }
            }
        }
        match last {
            AssetHead::Downloads => {}
            AssetHead::NotServed => {
                return Err(Error::new(format!(
                    "the public channel {slug} does not serve {name} to a client with no \
                     credential: an unauthenticated HEAD of {url} still answered 404 after \
                     {ANON_PROBE_ATTEMPTS} attempts over ~{}s. The authenticated checks \
                     passed, so the repository is private (GitHub answers 404 on this host \
                     for one) or the upload never completed. Make {slug} public, then \
                     `{CUT_COMMAND}`.",
                    u64::from(ANON_PROBE_ATTEMPTS) * ANON_PROBE_DELAY.as_secs(),
                )));
            }
            AssetHead::Inconclusive(detail) => {
                return Err(Error::new(format!(
                    "cannot prove a credential-less client can download {name} from {slug}: \
                     the last of {ANON_PROBE_ATTEMPTS} unauthenticated HEADs of {url} \
                     answered {detail} — not a redirect into GitHub's release-asset storage. \
                     Check the network, then `{CUT_COMMAND}`."
                )));
            }
            AssetHead::Refused(_) => unreachable!("a refusal returns inside the loop"),
        }
    }
    Ok(issued)
}

/// What the evergreen pointer (`https://github.com/<slug>/releases/latest/download/<asset>`)
/// answers a credential-less client, read with redirects refused by the deployed
/// web-lane updater's own resolver (`aterm_update_core::pointer::resolve`), so the
/// publisher cannot drift from the client it proves something about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PointerProbe {
    /// A 3xx whose `Location` is exactly this repository's tag-specific download URL
    /// for `asset` under a tag the deployed client accepts.
    Tag { tag: String, location: String },
    /// 404: no published (non-draft, non-prerelease) release — or a private repository,
    /// which GitHub renders identically on this host.
    NoRelease,
    /// 403 or 429: the host, or something in front of it, refused this client. Asked
    /// once: not convergence, and not the API rate limit on this unmetered host
    /// ([`anon_probe_refusal`]). Worded as the client words it.
    Refused { code: u16, reason: String },
    /// A release of this repository under a tag the client does not install from (a
    /// retired two-component tag, a suffixed tag, an `atpkg-index-<n>` cut): no app
    /// release. The head ratchet may take `latest` from it; the pointer gate, which
    /// requires this cut, refuses it.
    NotAnApp { tag: String },
    /// Anything else the client would not install this cut from — a refused redirect
    /// (another repository, a non-canonical tag), a non-redirect status, a 5xx —
    /// worded as the client words it.
    Other(String),
}

/// One anonymous, redirect-refusing HEAD of the evergreen pointer for `asset` in
/// `slug`, classified under `accept` (the deployed client's tag grammar). The core
/// resolver attaches no credential to any request, so this is what a stranger's
/// updater resolves. A transport failure is an error, never a verdict.
pub fn probe_evergreen_pointer(
    slug: &str,
    asset: &str,
    accept: &dyn Fn(&str) -> bool,
) -> Result<PointerProbe> {
    let (owner, repo) = slug
        .split_once('/')
        .ok_or_else(|| Error::new(format!("not an owner/repo slug: {slug}")))?;
    pointer_probe_from(
        slug,
        aterm_update_core::pointer::resolve(owner, repo, asset, accept),
    )
}

/// The client resolver's answer, mapped onto [`PointerProbe`]. Split out so the
/// mapping is testable without a network.
fn pointer_probe_from(
    slug: &str,
    resolved: std::result::Result<
        aterm_update_core::pointer::Pointer,
        aterm_update_core::pointer::PointerError,
    >,
) -> Result<PointerProbe> {
    use aterm_update_core::pointer::PointerError;
    match resolved {
        Ok(p) => Ok(PointerProbe::Tag {
            tag: p.tag,
            location: p.location,
        }),
        Err(PointerError::NoRelease { .. }) => Ok(PointerProbe::NoRelease),
        Err(PointerError::Transport(message)) => Err(Error::new(format!(
            "anonymous HEAD of the evergreen pointer of {slug} failed: {message}"
        ))),
        Err(PointerError::UnsafeName) => Err(Error::new(format!(
            "{slug} is not a URL-safe release source"
        ))),
        // `OtherTag`: a release of this repository under a tag the client does not
        // install from. The pointer gate still fails it; the head ratchet reads it as no
        // app head.
        Err(PointerError::OtherTag { tag }) => Ok(PointerProbe::NotAnApp { tag }),
        // A 429 is the client's `Transient` and a 403 its `Unexpected`; to the gate both
        // are the one thing it must not ask twice.
        Err(refused @ PointerError::Transient { code: 429, .. }) => Ok(PointerProbe::Refused {
            code: 429,
            reason: refused.to_string(),
        }),
        Err(refused @ PointerError::Unexpected { code: 403, .. }) => Ok(PointerProbe::Refused {
            code: 403,
            reason: refused.to_string(),
        }),
        Err(
            other @ (PointerError::Refused { .. }
            | PointerError::Transient { .. }
            | PointerError::Unexpected { .. }),
        ) => Ok(PointerProbe::Other(other.to_string())),
    }
}

/// The pointer gate. The deployed web-lane updater finds the head with one anonymous
/// HEAD of the evergreen appcast pointer, so a cut the pointer does not name reaches no
/// credential-less install. Proves, against what GitHub serves a stranger, that (1) the
/// pointer resolves to this cut's tag under the client's strict parse, (2) the appcast
/// there is byte-identical to the one uploaded, and (3) its `url` binds it to its tag
/// and container. Retried on its own bound ([`POINTER_PROBE_ATTEMPTS`] ×
/// [`POINTER_PROBE_DELAY`]); probe, fetch and sleep are injected for the fake GitHub in
/// `tests/it/channel_latest.rs`. Returns the served manifest.
///
/// # Errors
/// The pointer names another tag or nothing, a refusal, a served appcast that is not
/// this cut's bytes, or one whose `url` does not bind.
pub fn prove_pointer_serves(
    slug: &str,
    tag: &str,
    local_manifest: &[u8],
    probe: &mut dyn FnMut() -> Result<PointerProbe>,
    fetch: &mut dyn FnMut(&str) -> Result<Vec<u8>>,
    sleep: &mut dyn FnMut(Duration),
) -> Result<Manifest> {
    let asset = manifest_out::MANIFEST_ASSET;
    let mut last = PointerProbe::NoRelease;
    let mut location = None;
    for attempt in 1..=POINTER_PROBE_ATTEMPTS {
        last = probe()?;
        if let PointerProbe::Tag {
            tag: named,
            location: loc,
        } = &last
            && named == tag
        {
            location = Some(loc.clone());
            break;
        }
        // A refusal is asked once — see `PointerProbe::Refused`.
        if matches!(last, PointerProbe::Refused { .. }) {
            break;
        }
        if attempt < POINTER_PROBE_ATTEMPTS {
            sleep(POINTER_PROBE_DELAY);
        }
    }
    let Some(location) = location else {
        if let PointerProbe::Refused { code, reason } = &last {
            return Err(Error::new(format!(
                "the evergreen pointer https://github.com/{slug}/releases/latest/download/\
                 {asset} was REFUSED with HTTP {code} ({reason}). That host carries no API \
                 rate limit, so this is not the 60/hour budget: a 403 is a proxy, a firewall \
                 or an abuse block in front of GitHub, a 429 is GitHub throttling this \
                 address. Asked once, not retried; nothing about {tag} is known from it. Check \
                 from another network (`curl -sI` of that URL answers 302), then \
                 `{CUT_COMMAND}`."
            )));
        }
        let observed = match &last {
            PointerProbe::Tag { tag: named, .. } => format!("names {named}"),
            PointerProbe::NoRelease => {
                "answers 404 (no published non-prerelease release, or the repository is \
                 private)"
                    .to_string()
            }
            PointerProbe::NotAnApp { tag: named } => {
                format!("names {named}, a release of this channel that is not an app release")
            }
            PointerProbe::Other(reason) => format!("answers: {reason}"),
            PointerProbe::Refused { .. } => unreachable!("a refusal returned above"),
        };
        return Err(Error::new(format!(
            "the evergreen pointer https://github.com/{slug}/releases/latest/download/{asset} \
             {observed}, not {tag} — every credential-less install discovers the channel head \
             from that pointer, and this cut is not what it names. The cut sets it itself \
             (make_latest=true on this release, after its appcast pair is up); \
             `{CUT_COMMAND}` sends that again. If the pointer names a NEWER release, \
             this cut is not the channel head and must not be made one."
        )));
    };
    // (2) the bytes at the tag-specific URL the pointer named are the uploaded appcast.
    let served = fetch(&location)?;
    if served != local_manifest {
        return Err(Error::new(format!(
            "the appcast served at {location} ({} bytes) is not byte-identical to the one \
             this cut uploaded ({} bytes) — someone republished under {tag}, or the upload \
             was clobbered; investigate before trusting this release",
            served.len(),
            local_manifest.len(),
        )));
    }
    // (3) The URL bind: the web-lane client refuses a manifest whose `url` is not the
    // tag-specific download URL of its own `dmg`
    // (`aterm_update::github::web_container_url_agrees`), so a mis-set `update_channel`
    // fails this cut, not every credential-less install.
    let served_text = std::str::from_utf8(&served)
        .map_err(|_| Error::new(format!("the appcast served at {location} is not UTF-8")))?;
    let manifest = Manifest::parse(served_text).map_err(|error| {
        Error::new(format!(
            "the appcast served at {location} does not parse as a manifest: {error}"
        ))
    })?;
    assert_manifest_url_binds(slug, tag, &manifest)?;
    Ok(manifest)
}

/// [`prove_pointer_serves`] for this cut, over the real transport: the deployed client's
/// own resolver for the probe, an anonymous `curl` for the appcast.
fn prove_evergreen_pointer_serves_this_cut(ctx: &CutCtx, slug: &str) -> Result<()> {
    let asset = manifest_out::MANIFEST_ASSET;
    let local = fs::read(ctx.manifest_path()).map_err(|error| {
        Error::new(format!(
            "read journaled manifest {} for the pointer gate: {error}",
            ctx.manifest_path().display()
        ))
    })?;
    let manifest = prove_pointer_serves(
        slug,
        &ctx.tag,
        &local,
        &mut || {
            probe_evergreen_pointer(slug, asset, &aterm_update_core::pointer::canonical_app_tag)
        },
        &mut anonymous_get,
        &mut std::thread::sleep,
    )?;
    step(
        "",
        &format!(
            "evergreen pointer → {} and serves the uploaded {asset} byte-identically, whose \
             url binds it to {} and {}",
            ctx.tag, ctx.tag, manifest.dmg
        ),
    );
    Ok(())
}

/// One credential-free GET of a release download URL, redirects followed (https only),
/// retried on [`anon_probe`]'s budget. The bytes, `None` when the host still answers
/// 404 after that budget (the asset is not there), or an error naming the URL.
fn anonymous_get_optional(location: &str) -> Result<Option<Vec<u8>>> {
    let served = anon_probe(&[
        "--silent",
        "--show-error",
        "--fail",
        "--location",
        "--proto-redir",
        "=https",
        "--max-time",
        "60",
        location,
    ])?;
    if served.status.success() {
        return Ok(Some(served.stdout));
    }
    let stderr = String::from_utf8_lossy(&served.stderr);
    if http_status_from_curl_failure(&stderr) == Some(404) {
        return Ok(None);
    }
    Err(Error::new(format!(
        "an unauthenticated GET of {location} failed ({})",
        stderr.trim()
    )))
}

/// [`anonymous_get_optional`] for an asset that must be there.
fn anonymous_get(location: &str) -> Result<Vec<u8>> {
    anonymous_get_optional(location)?.ok_or_else(|| {
        Error::new(format!(
            "an unauthenticated GET of {location} answered 404 for the whole retry budget"
        ))
    })
}

/// The head ratchet: a cut takes the channel head (GitHub's `latest`) only from an
/// older release, since `make_latest` obeys no version order and a stale journal resumed
/// after a newer release shipped would hand installs an older build. `probe` resolves
/// the evergreen pointer and `fetch` GETs the appcast it names, as a stranger's updater
/// does. Passes when the pointer names this cut, nothing, or a release the client does
/// not install from (an older release serving no appcast included); otherwise the head
/// must sort strictly below this cut's tag and carry a strictly lower build.
///
/// # Errors
/// A newer (or equal-build) head, a head that cannot be established — a refusal, a
/// redirect out of this channel, a 5xx — or an appcast that cannot be read.
pub fn prove_channel_head_is_older(
    slug: &str,
    tag: &str,
    build: u64,
    probe: &mut dyn FnMut() -> Result<PointerProbe>,
    fetch: &mut dyn FnMut(&str) -> Result<Option<Vec<u8>>>,
) -> Result<()> {
    let (head, location) = match probe()? {
        // No published release, or none the client installs from: no app head.
        PointerProbe::NoRelease | PointerProbe::NotAnApp { .. } => return Ok(()),
        PointerProbe::Tag {
            tag: head,
            location,
        } => (head, location),
        PointerProbe::Refused { code, reason } => {
            return Err(Error::new(format!(
                "cannot establish which release holds {slug}'s `latest` before taking it: the \
                 evergreen pointer was REFUSED with HTTP {code} ({reason}); nothing was made \
                 the head. Check from another network, then `{CUT_COMMAND}`."
            )));
        }
        PointerProbe::Other(reason) => {
            return Err(Error::new(format!(
                "cannot establish which release holds {slug}'s `latest` before taking it: the \
                 evergreen pointer answers {reason}; nothing was made the head. Inspect the \
                 channel's latest release by hand, then `{CUT_COMMAND}`."
            )));
        }
    };
    if head == tag {
        return Ok(());
    }
    let newer = || {
        Error::new(format!(
            "{head} holds {slug}'s `latest`, and it is not older than this cut's {tag}: a cut \
             takes the channel head only from an OLDER release, so nothing was made the head. \
             A newer release reached the channel after this journal was written — find out \
             which cut published {head} before touching this journal; never hand `latest` \
             back to {tag} by hand."
        ))
    };
    if canonical_channel_tag_order(&head)? >= canonical_channel_tag_order(tag)? {
        return Err(newer());
    }
    // An older release with no appcast is no app head either (a source-only release
    // left holding `latest`); taking the pointer from it is the repair.
    let Some(bytes) = fetch(&location)? else {
        return Ok(());
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::new(format!("the appcast at {location} is not UTF-8")))?;
    let manifest = Manifest::parse(text).map_err(|error| {
        Error::new(format!(
            "the appcast at {location} does not parse as a manifest: {error}"
        ))
    })?;
    if manifest.build_number >= build {
        return Err(Error::new(format!(
            "{head} holds {slug}'s `latest` carrying build {}, which is not below this cut's \
             {build}: the fleet orders by build number, so making {tag} the head would hand it \
             a build it refuses to apply (or a downgrade). Refusing to take the pointer.",
            manifest.build_number
        )));
    }
    Ok(())
}

/// The publisher-side statement of the client's URL bind: `manifest.url` must be
/// exactly `aterm_update_core::cdn::release_download_url(owner, repo, tag, manifest.dmg)`.
fn assert_manifest_url_binds(slug: &str, tag: &str, manifest: &Manifest) -> Result<()> {
    let (owner, repo) = slug
        .split_once('/')
        .ok_or_else(|| Error::new(format!("not an owner/repo slug: {slug}")))?;
    let want = aterm_update_core::cdn::release_download_url(owner, repo, tag, &manifest.dmg)
        .ok_or_else(|| {
            Error::new(format!(
                "no download URL can be derived for {:?} of {tag} in {slug}",
                manifest.dmg
            ))
        })?;
    match manifest.url.as_deref() {
        Some(url) if url == want => Ok(()),
        other => Err(Error::new(format!(
            "the appcast's url is {}, not {want} — the credential-less updater refuses a \
             manifest whose url does not bind it to its tag and container; check \
             `[workspace.metadata.aterm] update_channel` (manifest_out.rs writes it into \
             `url`) before cutting again",
            other.map_or_else(|| "absent".to_string(), |url| format!("{url:?}"))
        ))),
    }
}

/// [`prove_assets_download_anonymously`] for this cut: every asset the deployed updater
/// elects, over the real transport, with the result in the transcript.
fn prove_channel_assets_download_anonymously(ctx: &CutCtx, slug: &str) -> Result<()> {
    let names = ctx.channel_asset_names();
    let issued = prove_assets_download_anonymously(
        slug,
        &ctx.tag,
        &names,
        &mut aterm_update_core::head_no_redirect,
        &mut std::thread::sleep,
    )?;
    step(
        "",
        &format!(
            "all {} required assets download with no credential from \
             github.com/{slug}/releases/download/{}/ ({issued} unmetered HEADs, no API request)",
            names.len(),
            ctx.tag
        ),
    );
    Ok(())
}

/// One direct REST draft-create on the channel, never [`gh_retry`] or the high-level
/// `gh release create` command: a client-side timeout can report failure for a create
/// that LANDED server-side, and GitHub happily mints a SECOND draft with the same
/// `tag_name` (drafts don't own their tag until published) — an orphan in front of the
/// whole fleet's channel. The durable intent is saved before the POST; this invocation
/// then probes once, and a later resume may discover but never recreate it.
///
/// It sends NO `target_commitish`: the claim commit lives on origin, not on the
/// channel, and naming it would either fail the POST or bind the release to an
/// unrelated object. GitHub anchors the tag at the channel's default branch when the
/// draft is published — the tag on the channel is a distribution marker, and the
/// authenticity of the bytes comes from the manifest digest, the pinned signature and
/// codesign, never from the release's target. (Every real cut normally ADOPTS the
/// engine's source release instead; this path is a channel `pub publish` did not
/// prepare.)
fn create_channel_draft(ctx: &mut CutCtx, slug: &str) -> Result<ReleaseObjectIdentity> {
    let notes = release_body_for_post("publish", &ctx.notes_path())?;
    let posted_body_len = notes.len();
    let title = format!("aterm {}", ctx.version);
    let endpoint = format!("{GITHUB_API_ORIGIN}/repos/{slug}/releases");
    let payload = release_create_payload(ctx.tag.as_str(), &title, &notes)?;
    let post = OneShotPost::prepare_json("create", "release request", &endpoint, &payload)?;
    // Every fallible preflight precedes the durable edge; the non-cloneable permit is
    // consumed by the POST that immediately follows.
    ensure_ctx_release_lease(ctx)?;
    let permit = ctx.persist_release_intent()?;
    // Creation is deliberately attempted at most once per invocation. A timeout
    // followed by an eventually-consistent empty list cannot prove the POST did not
    // land; retrying here can mint a duplicate draft.
    let out = post.issue(permit)?;
    if out.success() {
        let release = parse_release_object_response(&out.stdout)?;
        validate_channel_release_capability(Some(&release), release.id, &ctx.tag, true)?;
        return Ok(release);
    }
    if let Some(release) = unique_release_object_by_tag(slug, &ctx.tag)? {
        validate_channel_release_capability(Some(&release), release.id, &ctx.tag, true)?;
        return Ok(release);
    }
    // The server answered with a refusal: nothing was created, so release the intent
    // and let the next invocation post once more (`server_refused_without_creating`).
    if server_refused_without_creating(&out) {
        ctx.release_release_intent()?;
        return Err(Error::new(format!(
            "release create for {} was REFUSED by {slug} ({}); nothing was created, so the \
             release intent has been released and `--resume` may post once more.",
            ctx.tag,
            release_post_failure_detail(&out, posted_body_len)
        )));
    }
    Err(Error::new(format!(
        "release create returned failure and no exact release object is visible for {} on \
         {slug}; refusing an ambiguous retry in this invocation (resume after GitHub \
         converges): {}",
        ctx.tag,
        release_post_failure_detail(&out, posted_body_len)
    )))
}

/// Converge one exact-name asset onto the bound channel release under a durable
/// one-shot intent: a lost POST response can delay a resume but never duplicate or
/// overwrite an object. An existing object with the wrong bytes is never replaced
/// (stop and let a human look), except the roster pair on an adopted release (below).
fn upload_channel_asset(
    ctx: &mut CutCtx,
    slug: &str,
    release_id: u64,
    file: &Path,
    expect_draft: bool,
) -> Result<()> {
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
    let observed = release_asset_identity_for_release_id_optional(slug, release_id, name)?;
    match durable_post_decision(prior_intent, observed.is_some()) {
        DurablePostDecision::AwaitVisibility => {
            return Err(Error::new(format!(
                "upload intent for {name} was already durably issued, but the asset is not \
                 visible; refusing a duplicate POST (resume after GitHub converges)"
            )));
        }
        DurablePostDecision::ConvergeVisible => {
            match verify_release_asset_id_matches_local(slug, release_id, name, file) {
                Ok(_verified) => return Ok(()),
                // The one permitted replacement: on an adopted source release, the
                // source publish's own roster pair can be stale relative to the roster
                // this cut was built under. Gated three ways: the adopted path, the roster
                // pair, a mismatch. The stale object is deleted by its immutable ID and the
                // proven bytes go up through the same one-shot POST and re-download proof.
                Err(error)
                    if !expect_draft
                        && (name == roster::ROSTER_ASSET || name == roster::ROSTER_SIG_ASSET) =>
                {
                    let (stale_id, _stale_size) =
                        observed.as_ref().expect("ConvergeVisible implies observed");
                    step(
                        "publish",
                        &format!(
                            "replacing stale source-published {name} (asset ID {stale_id}) with \
                             this cut's bytes: {error}"
                        ),
                    );
                    ensure_ctx_release_lease(ctx)?;
                    let endpoint = format!("repos/{slug}/releases/assets/{stale_id}");
                    let out = gh_raw(&["api", "--method", "DELETE", &endpoint])?;
                    if !out.success() {
                        return Err(Error::new(format!(
                            "failed to delete stale {name} (asset ID {stale_id}) on {slug}: {}",
                            out.stderr_utf8().trim()
                        )));
                    }
                    // fall through to the one-shot POST below
                }
                Err(error) => {
                    return Err(Error::new(format!(
                        "release asset {name} on {slug} already exists with different bytes \
                         than this cut's artifact; refusing to overwrite a channel object. \
                         Inspect release ID {release_id} on {slug} by hand: {error}"
                    )));
                }
            }
        }
        DurablePostDecision::PersistIntentThenPost => {}
    }

    let endpoint = exact_release_upload_url(slug, release_id, name)?;
    let post = OneShotPost::prepare_binary("release asset", &endpoint, file)?;
    let release = release_object_by_id(slug, release_id)?;
    validate_channel_release_capability(release.as_ref(), release_id, &ctx.tag, expect_draft)?;
    ensure_ctx_release_lease(ctx)?;
    let permit = ctx.persist_upload_intent(name)?;
    // Like the create, an upload POST is issued once per invocation. An absent
    // immediate probe after a timeout may be visibility lag, not proof of
    // non-delivery; a resume first converges on any exact-name object.
    let out = post.issue(permit)?;
    // BEFORE ANY REMOTE PROBE. This verdict is local and provable, and the probe below
    // is a network call that can fail — under exactly the conditions that produce a
    // curl exit 2 in the first place (memory pressure kills the gh spawn; a hammered
    // API answers 5xx three times). A failed probe must not leave the intent standing:
    // that is the permanent wedge this retraction exists to prevent. The pre-POST probe
    // above already answered "did this asset exist beforehand".
    //
    // THE ANSWERED 4xx IS THE OTHER VERDICT THAT IS SPENT BEFORE ANY PROBE, and it
    // follows the release create's rule exactly: nothing was created, so the intent is
    // spent on nothing and must not wedge every resume at "refusing a duplicate POST" —
    // a token that expired mid-cut, a throttle, a refused upload. One best-effort probe
    // follows it ([`after_upload_post`]): `422 already_exists` is a 4xx too, and an
    // object that is there must stay named by the journal. A 5xx, a timeout or a reset
    // is neither verdict: each can hide a stored object, so each keeps the conservative
    // intent and goes on to the probe.
    let visible = || {
        release_asset_identity_for_release_id_optional(slug, release_id, name)
            .map(|found| found.is_some())
    };
    match after_upload_post(|| ctx.settle_upload_intent(name, &out), visible)? {
        AfterUploadPost::Converge { journal } => {
            verify_release_asset_id_matches_local(slug, release_id, name, file)?;
            if journal {
                // The POST was refused, its intent retracted — and yet this cut's own
                // bytes are there (a `422 already_exists` for an object nobody
                // journaled). Name it again, so `--abandon` withdraws it.
                step(
                    "publish",
                    &format!(
                        "upload of {name} was refused ({}) but the asset is on {slug} with this \
                         cut's bytes; converging on it and journaling it",
                        out.stderr_utf8().trim()
                    ),
                );
                ctx.record_upload_intent(name)?;
            }
            Ok(())
        }
        AfterUploadPost::NeverSent => Err(Error::new(format!(
            "upload of {name} never reached the network (curl exit {}): {}. The durable \
             intent was retracted, so a resume will retry it once the local cause is fixed",
            out.status,
            out.stderr_utf8().trim()
        ))),
        AfterUploadPost::Refused => {
            let said = github_refusal_summary(&out.stdout)
                .map(|said| format!(" — GitHub said: {said}"))
                .unwrap_or_default();
            Err(Error::new(format!(
                "upload of {name} was REFUSED by {slug} ({}{said}); nothing was created, so \
                 the durable intent was retracted and `--resume` may post it once more.",
                out.stderr_utf8().trim()
            )))
        }
        AfterUploadPost::Ambiguous => Err(Error::new(format!(
            "upload of {name} returned {} but no asset is visible on {slug}; refusing an \
             ambiguous duplicate retry in this invocation (resume after GitHub converges): {}",
            if out.success() { "success" } else { "failure" },
            out.stderr_utf8().trim()
        ))),
    }
}

/// Prove the bound release carries exactly the asset set this cut publishes (beside
/// the engine's source attestation pair), each object byte-identical to its artifact in
/// `dist/`: from a fresh listing, bracketed by two identical inventories so nothing was
/// swapped during the proof.
fn prove_channel_assets(
    ctx: &CutCtx,
    slug: &str,
    release_id: u64,
    expect_draft: bool,
) -> Result<()> {
    let before = release_object_by_id(slug, release_id)?;
    validate_channel_release_capability(before.as_ref(), release_id, &ctx.tag, expect_draft)?;
    let inventory_before = release_asset_inventory_for_release_id(slug, release_id)?;
    let names: Vec<String> = inventory_before
        .iter()
        .map(|asset| asset.name.clone())
        .collect();
    ctx.validate_channel_assets(&names)?;
    for file in ctx.channel_asset_paths() {
        let name = file
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| Error::new("release artifact filename is not UTF-8"))?;
        verify_release_asset_id_matches_local(slug, release_id, name, &file)?;
    }
    let inventory_after = release_asset_inventory_for_release_id(slug, release_id)?;
    if inventory_after != inventory_before {
        return Err(Error::new(
            "channel asset name/immutable-ID/size inventory changed during byte verification",
        ));
    }
    let after = release_object_by_id(slug, release_id)?;
    validate_channel_release_capability(after.as_ref(), release_id, &ctx.tag, expect_draft)?;
    Ok(())
}

/// Replay the deployed client's election against the live channel and require that it
/// lands on this cut: the acceptance test for the whole cut. Re-checks the elected
/// release's tag, its exact asset set, and the manifest bytes the client would
/// download.
fn prove_client_elects_this_cut(ctx: &CutCtx, slug: &str, release_id: u64) -> Result<()> {
    let live = release_object_by_id(slug, release_id)?;
    validate_channel_release_capability(live.as_ref(), release_id, &ctx.tag, false)?;
    let names: Vec<String> = release_asset_inventory_for_release_id(slug, release_id)?
        .into_iter()
        .map(|asset| asset.name)
        .collect();
    ctx.validate_channel_assets(&names)?;

    // `stop_early: true` is the client's replay: canonical tags only, exact
    // `aterm-appcast.toml` only, greatest numeric tag wins regardless of REST row
    // order — and it downloads exactly the one manifest a real updater would fetch.
    let published = verify::scan_published(slug, true)?;
    let head = published.first().ok_or_else(|| {
        Error::new(format!(
            "the channel {slug} elects no release at all after publishing v{} — installed \
             copies would still report no update",
            ctx.version
        ))
    })?;
    if head.tag != ctx.tag {
        return Err(Error::new(format!(
            "the channel {slug} elects {}, not this cut's {}; the fleet would install a \
             different build than the one just proved",
            head.tag, ctx.tag
        )));
    }
    if head.version != ctx.version || head.build != ctx.build {
        return Err(Error::new(format!(
            "the manifest the channel {slug} serves carries v{} build {}, not this cut's v{} \
             build {}",
            head.version, head.build, ctx.version, ctx.build
        )));
    }
    if head.asset != manifest_out::MANIFEST_ASSET {
        return Err(Error::new(format!(
            "the channel {slug} head resolved through asset {:?}, not the exact {} the client \
             requires",
            head.asset,
            manifest_out::MANIFEST_ASSET
        )));
    }
    let local_manifest = fs::read_to_string(ctx.manifest_path()).map_err(|error| {
        Error::new(format!(
            "read local manifest for the head proof {}: {error}",
            ctx.manifest_path().display()
        ))
    })?;
    if head.text != local_manifest {
        return Err(Error::new(format!(
            "the manifest served by the channel {slug} is not byte-identical to this cut's \
             dist/{}",
            manifest_out::MANIFEST_ASSET
        )));
    }
    Ok(())
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

#[cfg(test)]
mod release_body_post_tests {
    //! The body the POST carries, and what a refusal of it says. GitHub takes 125,000
    //! characters, and the file read at the POST can come from an older binary, a
    //! resumed cut or an operator's hand, so the bound is taken on the operation.

    use super::*;

    fn notes_file(label: &str, contents: &str) -> (PrivateTempDir, PathBuf) {
        let sequence = RELEASE_ASSET_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let dir = PrivateTempDir::create(std::env::temp_dir().join(format!(
            "aterm-release-notes-{label}-{}-{sequence}",
            std::process::id()
        )))
        .expect("create release-notes test directory");
        let path = dir.path().join("notes-0.87.0.md");
        fs::write(&path, contents).expect("write release-notes fixture");
        (dir, path)
    }

    /// An over-limit file on disk, whatever wrote it, is posted bounded; a raw read of
    /// the file fails this.
    #[test]
    fn an_oversized_notes_file_on_disk_is_bounded_at_the_post() {
        let oversized = "### Section\n\n- an entry long enough to matter\n\n".repeat(6000);
        assert!(
            oversized.len() > changelog::GITHUB_RELEASE_BODY_LIMIT,
            "fixture must actually overflow: {} bytes",
            oversized.len()
        );
        let (_dir, path) = notes_file("oversized", &oversized);
        let posted = release_body_for_post("draft", &path).expect("read notes");
        assert!(
            posted.len() <= changelog::GITHUB_RELEASE_BODY_LIMIT,
            "the POST would carry {} bytes, over the {} GitHub takes",
            posted.len(),
            changelog::GITHUB_RELEASE_BODY_LIMIT
        );
        // What is sent is that bounded body: the request is built from it alone.
        let payload = release_create_payload("v0.87.0", "aterm 0.87.0", &posted).unwrap();
        let request: aterm_json::Value =
            aterm_json::from_str(std::str::from_utf8(&payload).unwrap()).unwrap();
        assert_eq!(
            request.get("body").and_then(aterm_json::Value::as_str),
            Some(&*posted)
        );
        assert_eq!(
            request.get("draft").and_then(aterm_json::Value::as_bool),
            Some(true)
        );
        // The file itself is untouched: the bound is on what is sent.
        assert_eq!(
            fs::read_to_string(&path).expect("re-read fixture").len(),
            oversized.len()
        );
    }

    /// No release asset is read by name: `gh_raw` refuses `gh release download`
    /// wherever it sits in the argv, before anything runs.
    #[test]
    fn a_name_based_asset_download_is_refused_before_gh_runs() {
        for args in [
            &["release", "download", "v0.87.0", "--pattern", "aterm.dmg"][..],
            &["-R", "alabsystems/aterm", "release", "download", "v0.87.0"],
        ] {
            let Err(refused) = gh_raw(args) else {
                panic!("a download by name ran: {args:?}");
            };
            assert!(refused.to_string().contains("by its id"), "{refused}");
        }
        assert!(!downloads_by_name(&["api", "repos/o/r/releases/assets/7"]));
        assert!(!downloads_by_name(&["release", "view", "download"]));
    }
}

#[cfg(test)]
mod transport_body_tests {
    //! The asset leg's memory cost: `--data-binary @file` buffers the whole payload,
    //! and an asset can be gigabytes. These assert the argv, the only artifact of the
    //! decision.

    use super::*;

    const AUTH: &str = "@/private/tmp/headers";
    const ENDPOINT: &str = "https://uploads.github.com/repos/o/r/releases/1/assets?name=x.dmg";

    #[test]
    fn an_asset_upload_streams_off_disk_and_never_buffers() {
        let args = OneShotPost::curl_args(
            AUTH,
            "Content-Type: application/octet-stream",
            BodySource::Streamed("/dist/aterm-0.33.0.dmg"),
            ENDPOINT,
        );
        assert!(
            args.iter().any(|a| a == "--upload-file"),
            "the asset leg must stream: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--data-binary"),
            "a gigabyte payload must never be buffered: {args:?}"
        );
        // `--upload-file` PUTs by default; the API needs POST.
        let request = args.iter().position(|a| a == "--request").unwrap();
        assert_eq!(args[request + 1], "POST");
        // No `@` prefix on this one: that spelling belongs to --data-binary, and
        // curl would look for a file literally named "@/dist/…".
        let flag = args.iter().position(|a| a == "--upload-file").unwrap();
        assert_eq!(args[flag + 1], "/dist/aterm-0.33.0.dmg");

        // A JSON body still buffers from its private temp file.
        let args = OneShotPost::curl_args(
            AUTH,
            "Content-Type: application/json",
            BodySource::Buffered("@/private/tmp/request.json"),
            ENDPOINT,
        );
        let flag = args.iter().position(|a| a == "--data-binary").unwrap();
        assert_eq!(args[flag + 1], "@/private/tmp/request.json");
        assert!(!args.iter().any(|a| a == "--upload-file"));
    }

    fn out(status: i32, stderr: &str) -> RunOut {
        RunOut {
            status,
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    /// The one case where "did the server see it?" is knowable locally. Everything
    /// else must stay conservative, because each of those can hide a delivered
    /// request — and a wrong `true` here would repeat a POST that landed.
    #[test]
    fn only_a_local_curl_refusal_counts_as_never_sent() {
        assert!(
            transport_never_started(&out(2, "curl: option --data-binary: out of memory")),
            "curl exit 2 is argument/init failure, strictly before connect"
        );
        for (status, why) in [
            (0, "success"),
            (22, "HTTP error returned"),
            (28, "timeout — the request may have been delivered"),
            (56, "recv failure — likewise"),
            (7, "connect failed, but after the attempt began"),
            (-1, "killed; unknowable"),
        ] {
            assert!(
                !transport_never_started(&out(status, why)),
                "exit {status} ({why}) must NOT license a retry"
            );
        }
    }

    /// An answered 4xx created nothing and reads as a refusal; everything else may have
    /// created something and reads as unknown.
    #[test]
    fn a_four_hundred_is_a_refusal_and_everything_else_is_unknown() {
        let out = |status: i32, stderr: &str| RunOut {
            status,
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        };
        let refused = out(22, "curl: (22) The requested URL returned error: 422\n");
        assert!(
            server_refused_without_creating(&refused),
            "an answered 422 created nothing"
        );
        assert!(
            server_refused_without_creating(&out(
                22,
                "curl: (22) The requested URL returned error: 404\n"
            )),
            "so did an answered 404"
        );
        // Everything that can hide a delivered request keeps the conservative reading.
        for unknown in [
            out(22, "curl: (22) The requested URL returned error: 500\n"),
            out(22, "curl: (22) The requested URL returned error: 502\n"),
            out(28, "curl: (28) Operation timed out\n"),
            out(56, "curl: (56) Recv failure: Connection reset by peer\n"),
            out(
                22,
                "curl: (22) some future wording this code has never seen\n",
            ),
            out(2, "curl: (2) failed to initialise\n"),
        ] {
            assert!(
                !server_refused_without_creating(&unknown),
                "{:?} must read as UNKNOWN, not as a refusal",
                unknown.stderr_utf8().trim()
            );
        }
        assert_eq!(
            http_status_from_curl_failure("curl: (22) The requested URL returned error: 422\n"),
            Some(422)
        );
        assert_eq!(http_status_from_curl_failure("no status here"), None);
    }

    /// A retracted intent returns the decision to "persist and post", not "await
    /// visibility".
    #[test]
    fn a_retracted_intent_makes_the_next_attempt_postable_again() {
        assert_eq!(
            durable_post_decision(true, false),
            DurablePostDecision::AwaitVisibility,
            "the wedge this fixes"
        );
        assert_eq!(
            durable_post_decision(false, false),
            DurablePostDecision::PersistIntentThenPost,
            "after retraction the asset is uploadable again"
        );
        assert_eq!(
            durable_post_decision(false, true),
            DurablePostDecision::ConvergeVisible,
            "and a visible object still converges rather than reposting"
        );
    }
}

#[cfg(test)]
mod roster_wiring_tests {
    //! The producer-side attach path: whether an armed cut is attributed and carries
    //! its roster at all. In-module because every property is a private method of
    //! [`CutCtx`], and a regression yields a well-formed release with no `machine_id`
    //! or no `aterm-machines.toml`, which an armed client refuses structurally with no
    //! fallback release.

    use super::*;

    /// A cut signs with this machine's key and verifies published bytes under the key
    /// that signed them: a cross-machine recovery verifies Ka while signing with Kb.
    #[test]
    fn a_recovery_verifies_under_the_release_key_and_signs_under_its_own() {
        let mut ctx = ctx(Some("m3"));
        ctx.signature_pubkey = Some("Kb-this-machine".to_string());
        assert_eq!(
            ctx.verification_pubkey(),
            Some("Kb-this-machine"),
            "an ordinary cut verifies under the key it signs with"
        );

        // A cross-machine recovery: the release was signed by the dead publisher.
        ctx.verify_pubkey = Some("Ka-dead-publisher".to_string());
        assert_eq!(
            ctx.verification_pubkey(),
            Some("Ka-dead-publisher"),
            "publish must prove the key that actually signed the artifacts"
        );
        assert_eq!(
            ctx.signature_pubkey.as_deref(),
            Some("Kb-this-machine"),
            "…while the local signing-configuration guards keep comparing this \
             machine's own key, or they would reject their own recovery"
        );
    }

    /// A context shaped like a real cut, differing only in whether it is attributed.
    /// Every remote-facing field is inert; nothing here touches the network or a repo.
    fn ctx(machine_id: Option<&str>) -> CutCtx {
        CutCtx {
            credentials: None,
            apple: sign::AppleTier::Inactive,
            repo: PathBuf::from("/nonexistent/repo"),
            tree: PathBuf::from("/nonexistent/repo"),
            dist: PathBuf::from("/nonexistent/repo/dist"),
            journal_path: PathBuf::from("/nonexistent/repo/dist/cut-state.toml"),
            slug: "owner/repo".to_string(),
            version: "0.5.0".to_string(),
            tag: "v0.5.0".to_string(),
            build: 500,
            commit: "a".repeat(40),
            min_build: None,
            artifact: Some("aa".repeat(32)),
            macho_sha256: Some("bb".repeat(32)),
            linux: None,
            manifest_signed: true,
            signature_required: true,
            signature_pubkey: None,
            verify_pubkey: None,
            signature_machine_id: machine_id.map(str::to_string),
            attribution: None,
            roster: None,
            release_id: None,
            release_intent: false,
            upload_intents: Vec::new(),
            kind: CutKind::Real,
            lease: None,
            fence: None,
            journal: None,
            notes_section: "0.5.0".to_string(),
        }
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .filter_map(|p| p.file_name()?.to_str().map(str::to_string))
            .collect()
    }

    /// An attributed cut attaches the roster to every asset set; an unattributed one (a
    /// fork's, or a resume past `sign`) attaches it to nothing.
    #[test]
    fn an_attributed_cut_carries_its_roster_through_every_asset_set() {
        let rostered = ctx(Some("m3"));
        assert!(rostered.attaches_roster());
        for (label, set) in [
            ("channel upload", rostered.channel_asset_paths()),
            (
                "channel acceptance",
                rostered
                    .channel_asset_names()
                    .iter()
                    .map(PathBuf::from)
                    .collect(),
            ),
        ] {
            let set = names(&set);
            assert!(
                set.contains(&roster::ROSTER_ASSET.to_string()),
                "the {label} set must carry the roster: {set:?}"
            );
            assert!(
                set.contains(&roster::ROSTER_SIG_ASSET.to_string()),
                "the {label} set must carry the roster signature: {set:?}"
            );
            // Precondition, so the two assertions above are not passing on an
            // accidentally-everything set: the ordinary artifacts are still there.
            assert!(set.contains(&"aterm-appcast.toml".to_string()), "{set:?}");
        }

        // The fork path (no master pinned): an unattributed cut attaches no roster.
        let plain = ctx(None);
        assert!(!plain.attaches_roster());
        let set = names(&plain.channel_asset_paths());
        assert!(
            !set.iter().any(|n| n.starts_with("aterm-machines")),
            "an unattributed cut must publish no roster: {set:?}"
        );
    }

    /// THE PIN EXPECTATION IS THE COMMITTED PAPER MASTER, whoever signs.
    ///
    /// `aterm-gui/build.rs` embeds `__DATA,__aterm_upin` from `PAPER_MASTER_PUBKEYS[0]`,
    /// so that is what a shipped binary can prove. The build (`artifact::build`) holds
    /// every slice to it, `step_sign` writes the fingerprint into the provenance, and
    /// `step_selfcheck` then
    /// checks the binary's `--diagnose` line and the provenance against it — both through
    /// `expected_embedded_pin`, so the two can never state different expectations.
    ///
    /// Negative control: the SIGNING key's fingerprint is a different string, so an
    /// implementation that derived the pin from `signature_pubkey` fails the last assert.
    #[test]
    fn the_pin_expectation_is_the_committed_master_whoever_signs() {
        let master = aterm_update_core::pins::PAPER_MASTER_PUBKEYS
            .first()
            .copied()
            .expect("precondition: this tree pins a paper master");
        // Two obviously-synthetic signing keys (base64 of thirty-two 0x42 / 0x43 bytes).
        let one = "QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkI=";
        let two = "Q0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0M=";
        let mut signing_one = ctx(None);
        signing_one.signature_pubkey = Some(one.to_string());
        let mut signing_two = ctx(None);
        signing_two.signature_pubkey = Some(two.to_string());
        let expected = signing_one.expected_embedded_pin().unwrap();
        assert_eq!(
            expected,
            signing_two.expected_embedded_pin().unwrap(),
            "the binary embeds the COMMITTED anchor, so the cutting machine cannot move \
             what the build and the self-check expect of it"
        );
        assert_eq!(expected, Some(update_key_fingerprint(master).unwrap()));
        assert_eq!(
            expected_embedded_update_pin(&[]).unwrap(),
            None,
            "a fork with no master has no anchor to prove"
        );
        assert_ne!(
            expected,
            Some(update_key_fingerprint(one).unwrap()),
            "deriving from the signer is the trap; the two must be distinguishable"
        );
    }

    /// The two readings the ratchet stands on, the channel's generation and this cut's,
    /// both from manifest bytes and `None` only when no roster is in play (a `None`
    /// passes `roster_floor_covered`).
    #[test]
    fn the_ratchet_reads_both_generations_out_of_manifest_bytes() {
        let dir = std::env::temp_dir().join(format!("aterm-ratchet-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut manifest = manifest_out::build(&manifest_out::ManifestInputs {
            version: "0.5.0",
            build_number: 500,
            commit: &"a".repeat(40),
            dmg_name: "aterm-0.5.0.dmg",
            dmg_sha256: &"ab".repeat(32),
            zip_name: "aterm-0.5.0-mac.zip",
            zip_sha256: &"cd".repeat(32),
            repo_slug: "owner/repo",
            min_os: "11.0",
            team_id: "",
            pub_date: "2026-08-11T00:00:00Z",
            min_build: None,
            changelog: "### Added\n- a thing\n",
        });
        let unattributed = manifest.to_toml().unwrap();
        machines::attribute(
            &mut manifest,
            &roster::Attribution {
                machine_id: "m3".to_string(),
                pubkey_b64: "k".to_string(),
                roster_seq: 7,
            },
        );
        let attributed = manifest.to_toml().unwrap();

        let head = |text: &str| verify::Published {
            release_id: Some(1),
            release: None,
            tag: "v0.5.0".to_string(),
            build: 500,
            version: "0.5.0".to_string(),
            asset: manifest_out::MANIFEST_ASSET.to_string(),
            min_build: None,
            text: text.to_string(),
        };
        assert_eq!(published_roster_seq(None).unwrap(), None);
        assert_eq!(
            published_roster_seq(Some(&head(&unattributed))).unwrap(),
            None,
            "an unrostered head imposes no floor — the fork state"
        );
        assert_eq!(
            published_roster_seq(Some(&head(&attributed))).unwrap(),
            Some(7)
        );

        // The cut's side: before `sign` the gate's attribution; after it, the staged
        // manifest.
        let mut fresh = ctx(Some("m3"));
        fresh.attribution = Some(roster::Attribution {
            machine_id: "m3".to_string(),
            pubkey_b64: "k".to_string(),
            roster_seq: 7,
        });
        assert_eq!(cut_roster_seq(&fresh).unwrap(), Some(7));

        let mut resumed = ctx(Some("m3"));
        resumed.dist = dir.clone();
        fs::write(dir.join(manifest_out::MANIFEST_ASSET), &attributed).unwrap();
        assert_eq!(
            cut_roster_seq(&resumed).unwrap(),
            Some(7),
            "a resume past build carries no attribution and must read its own manifest"
        );
        // An unattributed cut asks nothing of the filesystem and imposes no floor.
        assert_eq!(cut_roster_seq(&ctx(None)).unwrap(), None);
        // A rostered cut that can answer neither is an error, never a silent `None`:
        // `None` reads as "no roster in play" and would pass the ratchet.
        let mut lost = ctx(Some("m3"));
        lost.dist = dir.join("gone");
        assert!(cut_roster_seq(&lost).is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    /// A recovery reconstructs the roster pair from what the PUBLISHED MANIFEST says
    /// about itself, so the set it rebuilds is the set `channel_asset_paths` will demand.
    ///
    /// Kills the mutation "reconstruct nothing": the two sets then disagree, and
    /// `publish` dies on a file recovery never wrote — with advice naming the command
    /// that was already running.
    #[test]
    fn recovery_rebuilds_exactly_the_roster_assets_publish_will_demand() {
        let mut manifest = manifest_out::build(&manifest_out::ManifestInputs {
            version: "0.5.0",
            build_number: 500,
            commit: &"a".repeat(40),
            dmg_name: "aterm-0.5.0.dmg",
            dmg_sha256: &"ab".repeat(32),
            zip_name: "aterm-0.5.0-mac.zip",
            zip_sha256: &"cd".repeat(32),
            repo_slug: "owner/repo",
            min_os: "11.0",
            team_id: "",
            pub_date: "2026-08-11T00:00:00Z",
            min_build: None,
            changelog: "### Added\n- a thing\n",
        });
        // An unattributed published release reconstructs no roster, and demands none.
        assert!(recovered_roster_asset_names(&manifest).is_empty());
        assert!(
            !names(&ctx(manifest.machine_id.as_deref()).channel_asset_paths())
                .iter()
                .any(|name| name.starts_with("aterm-machines"))
        );

        machines::attribute(
            &mut manifest,
            &roster::Attribution {
                machine_id: "m3".to_string(),
                pubkey_b64: "k".to_string(),
                roster_seq: 4,
            },
        );
        let rebuilt: Vec<String> = recovered_roster_asset_names(&manifest)
            .into_iter()
            .map(str::to_string)
            .collect();
        let demanded = names(&ctx(manifest.machine_id.as_deref()).channel_asset_paths());
        for name in &rebuilt {
            assert!(demanded.contains(name), "{name} rebuilt but not demanded");
        }
        for name in demanded.iter().filter(|n| n.starts_with("aterm-machines")) {
            assert!(rebuilt.contains(name), "{name} demanded but never rebuilt");
        }
        assert_eq!(rebuilt.len(), 2, "{rebuilt:?}");
    }

    /// A recovery must not retire a newer local roster, the only copy of the machine's
    /// authorizing roster (`refuse_roster_downgrade`). The fixture is the real
    /// generation-2 pair, which verifies under the pinned paper master.
    #[test]
    fn recovery_refuses_to_overwrite_a_newer_local_roster() {
        let dir =
            std::env::temp_dir().join(format!("aterm-roster-downgrade-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Nothing local: there is nothing to protect, so a recovery proceeds.
        assert!(refuse_roster_downgrade(&dir, 1).is_ok());

        let bytes = aterm_codec::base64::decode_strict(ROSTER_SEQ2.concat().as_bytes()).unwrap();
        let sig = aterm_codec::base64::decode_strict(ROSTER_SEQ2_SIG.as_bytes()).unwrap();
        std::fs::write(dir.join(roster::ROSTER_ASSET), &bytes).unwrap();
        std::fs::write(dir.join(roster::ROSTER_SIG_ASSET), &sig).unwrap();

        // An older release: refused, naming the file it just protected.
        let err = refuse_roster_downgrade(&dir, 1).unwrap_err();
        assert!(err.0.contains("NEWER"), "{}", err.0);
        assert!(err.0.contains(roster::ROSTER_ASSET), "{}", err.0);

        // Same generation, or a newer one: nothing is being lost.
        assert!(refuse_roster_downgrade(&dir, 2).is_ok());
        assert!(refuse_roster_downgrade(&dir, 3).is_ok());

        // A pair that does not verify under the pinned master is not an authorizing
        // document, and must never be able to veto un-wedging the release pipeline.
        std::fs::write(dir.join(roster::ROSTER_SIG_ASSET), [0u8; 64]).unwrap();
        assert!(refuse_roster_downgrade(&dir, 1).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A release that names no machine is a fork's (no master pinned), so the policy's own
    /// key stays the authority. This path must not touch the network — it returns
    /// before any asset download.
    #[test]
    fn an_unattributed_release_keeps_the_policy_key() {
        let manifest = b"schema = 1\nversion = \"0.20.0\"\nbuild_number = 1786405661\n\
sha256 = \"aa\"\ndmg = \"aterm-0.20.0.dmg\"\n";
        let resolved = published_manifest_signature_pubkey(
            "unused/slug",
            0,
            manifest,
            Some("cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8="),
        )
        .expect("an unattributed manifest resolves without any download");
        assert_eq!(
            resolved.as_deref(),
            Some("cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=")
        );
    }

    /// The real generation-2 machine roster published on the update channel, base64 so
    /// the master signature covers the exact bytes.
    const ROSTER_SEQ2: &[&str] = &[
        "c2NoZW1hID0gMQpyb3N0ZXJfc2VxID0gMgp2YWxpZF91bnRpbCA9ICI5OTk5LTEyLTMxVDAwOjAwOjAwWiIKcmV2",
        "b2tlZCA9IFtdCgpbW21hY2hpbmVdXQppZCA9ICJpbmN1bWJlbnQtaGVhZCIKcHVia2V5ID0gImN3NWdJR1lRelg2",
        "eHJoVFhqWFU5bllmTFdlb0lraVoxeVVYN2Qxd21kejg9IgphZGRlZF9hdCA9ICIyMDI2LTA4LTE1VDIxOjU1OjA0",
        "WiIKCltbbWFjaGluZV1dCmlkID0gIm0zIgpwdWJrZXkgPSAiWU9IdzBPb2VmUTc5TmRFOHFzUUZvYklNUjdRWENo",
        "cHJlWUJpMk9mNzRVbz0iCmFkZGVkX2F0ID0gIjIwMjYtMDgtMTVUMjE6NTU6MDRaIgo=",
    ];
    const ROSTER_SEQ2_SIG: &str =
        "vNOvNYPssbUN3F/SmnoPDk6za2BAaewu9Vopl5YU7EDd+KUM0Y84eUryvFE9OWUywT/yggXE92SYQ2Qz7k56DA==";
}

#[cfg(test)]
mod lean_dmg_ceiling_tests {
    use super::*;

    /// Measured figures, so the boundary is pinned to real images.
    const LEAN_0_67_0: u64 = 31_018_076; // the shipped lean image
    const SEEDED_0_63_0: u64 = 1_067_044_990; // a seeded image

    /// REGRESSION v0.63.0: a stale cutter shipped a 1.07 GB seeded image, which the
    /// client's 2 GiB download bound accepted. The lean ceiling refuses that image at
    /// the packaging step, and passes the shipped lean one.
    #[test]
    fn the_v0_63_0_image_is_refused_and_the_lean_one_passes() {
        assert!(validate_lean_dmg_size("0.67.0", LEAN_0_67_0).is_ok());
        assert!(
            validate_release_asset_download_size(SEEDED_0_63_0).is_ok(),
            "the client bound alone let v0.63.0 through"
        );
        let err = validate_lean_dmg_size("0.95.0", SEEDED_0_63_0)
            .expect_err("a seeded image must be refused");
        assert!(err.to_string().contains("aterm-0.95.0.dmg"), "{err}");
    }

    fn sparse_dmg(label: &str, size: u64) -> (PrivateTempDir, PathBuf) {
        let sequence = RELEASE_ASSET_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let dir = PrivateTempDir::create(std::env::temp_dir().join(format!(
            "aterm-lean-dmg-{label}-{}-{sequence}",
            std::process::id()
        )))
        .expect("create lean-DMG test directory");
        let path = dir.path().join("aterm-test.dmg");
        let file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .expect("create sparse test DMG");
        file.set_len(size).expect("set sparse test DMG size");
        (dir, path)
    }

    #[test]
    fn the_boundary_is_inclusive() {
        assert!(validate_lean_dmg_size("0.64.0", LEAN_DMG_CEILING_BYTES).is_ok());
        assert!(validate_lean_dmg_size("0.64.0", LEAN_DMG_CEILING_BYTES + 1).is_err());
        // A zero-byte image is the download bound's business, not this one's.
        assert!(validate_lean_dmg_size("0.64.0", 0).is_ok());
        assert!(validate_release_asset_download_size(0).is_err());
    }

    /// A resumed self-check refuses an oversized dist/ DMG from the file's own
    /// metadata, whatever `sign` journaled.
    #[test]
    fn an_oversized_resumed_dmg_is_refused_from_the_actual_file() {
        let (_dir, path) = sparse_dmg("resume", LEAN_DMG_CEILING_BYTES + 1);
        validate_lean_dmg_on_disk(&path, "self-check", Some("abandon"))
            .expect_err("resume must re-check the current dist DMG");
    }

    /// Recovery judges the retained file's real size, not the size the download
    /// reported.
    #[test]
    fn an_oversized_recovered_dmg_is_refused_before_publish() {
        let sequence = RELEASE_ASSET_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let dir = PrivateTempDir::create(std::env::temp_dir().join(format!(
            "aterm-lean-dmg-recovery-{}-{sequence}",
            std::process::id()
        )))
        .expect("create recovered-DMG test directory");
        let path = dir.path().join("aterm-recovered.dmg");
        recover_lean_dmg_to(&path, |destination| {
            let file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(destination)
                .map_err(|error| Error::new(format!("create recovered test DMG: {error}")))?;
            file.set_len(LEAN_DMG_CEILING_BYTES + 1)
                .map_err(|error| Error::new(format!("size recovered test DMG: {error}")))?;
            Ok(VerifiedReleaseAsset {
                id: 7,
                // Deliberately lie: the post-retention filesystem observation,
                // not this historical/API field, must make the decision.
                size: 1,
                sha256: "00".repeat(32),
            })
        })
        .expect_err("recovery must reject the retained seeded image");
    }

    /// The pointer probe is a thin map over the client's resolver: a tag is a tag, a 404
    /// is "no release", a transport failure is an error (never a verdict), a non-app tag
    /// and a 403/429 have their own arms, and anything else is `Other` in the client's
    /// words.
    #[test]
    fn the_pointer_probe_preserves_the_client_resolvers_three_arms() {
        use aterm_update_core::pointer::{Pointer, PointerError};
        let good =
            "https://github.com/alabsystems/aterm/releases/download/v0.74.0/aterm-appcast.toml";
        assert_eq!(
            pointer_probe_from(
                "alabsystems/aterm",
                Ok(Pointer {
                    tag: "v0.74.0".into(),
                    location: good.into()
                })
            )
            .unwrap(),
            PointerProbe::Tag {
                tag: "v0.74.0".into(),
                location: good.into()
            }
        );
        assert_eq!(
            pointer_probe_from(
                "alabsystems/aterm",
                Err(PointerError::NoRelease { url: "x".into() })
            )
            .unwrap(),
            PointerProbe::NoRelease
        );
        assert!(
            pointer_probe_from(
                "alabsystems/aterm",
                Err(PointerError::Transport("curl: (6) DNS".into()))
            )
            .is_err(),
            "a transport failure is an error, never a verdict"
        );
        assert!(pointer_probe_from("alabsystems/aterm", Err(PointerError::UnsafeName)).is_err());
        for refused in [
            PointerError::Refused {
                why: "the redirect's tag is not a release tag this client installs from",
            },
            PointerError::Transient {
                code: 503,
                url: "x".into(),
            },
            PointerError::Unexpected {
                code: 200,
                url: "x".into(),
            },
        ] {
            let text = refused.to_string();
            assert_eq!(
                pointer_probe_from("alabsystems/aterm", Err(refused)).unwrap(),
                PointerProbe::Other(text)
            );
        }
        // The pointer captured by a non-app release of this same channel:
        // its own arm, so the head ratchet can read "no app head" while the pointer gate
        // still refuses to call it this cut.
        assert_eq!(
            pointer_probe_from(
                "alabsystems/aterm",
                Err(PointerError::OtherTag {
                    tag: "atpkg-index-30".into(),
                })
            )
            .unwrap(),
            PointerProbe::NotAnApp {
                tag: "atpkg-index-30".into()
            }
        );
        // A 429 and a 403 are refusals, asked once, carrying the status. A 5xx and a
        // bare 200 stay `Other` above: the negative control.
        for (code, refused) in [
            (
                429,
                PointerError::Transient {
                    code: 429,
                    url: "x".into(),
                },
            ),
            (
                403,
                PointerError::Unexpected {
                    code: 403,
                    url: "x".into(),
                },
            ),
        ] {
            let reason = refused.to_string();
            assert_eq!(
                pointer_probe_from("alabsystems/aterm", Err(refused)).unwrap(),
                PointerProbe::Refused { code, reason }
            );
        }
    }

    /// The cut refuses an appcast whose `url` is not the client's derived download URL
    /// for its tag and container — the bind the web-lane client enforces, proven at
    /// the publisher so a mis-set `update_channel` fails the cut and not the fleet.
    #[test]
    fn the_cut_asserts_the_manifests_url_bind() {
        let parse = |url: &str| {
            Manifest::parse(&format!(
                "schema = 1\nversion = \"0.74.0\"\nbuild_number = 74\n\
                 sha256 = \"{}\"\ndmg = \"aterm-0.74.0.dmg\"\n{url}",
                "ab".repeat(32)
            ))
            .unwrap()
        };
        let bound = parse(
            "url = \"https://github.com/alabsystems/aterm/releases/download/v0.74.0/aterm-0.74.0.dmg\"\n",
        );
        assert_manifest_url_binds("alabsystems/aterm", "v0.74.0", &bound).unwrap();
        for (why, url) in [
            ("absent", ""),
            (
                "another channel",
                "url = \"https://github.com/private/origin/releases/download/v0.74.0/aterm-0.74.0.dmg\"\n",
            ),
            (
                "another tag",
                "url = \"https://github.com/alabsystems/aterm/releases/download/v0.73.0/aterm-0.74.0.dmg\"\n",
            ),
            (
                "another container",
                "url = \"https://github.com/alabsystems/aterm/releases/download/v0.74.0/aterm.dmg\"\n",
            ),
        ] {
            assert_manifest_url_binds("alabsystems/aterm", "v0.74.0", &parse(url)).expect_err(why);
        }
    }
}

#[cfg(test)]
mod anonymous_readability_tests {
    //! The publish step spends nothing of the anonymous API budget: one credential-free
    //! HEAD per required asset on the unmetered download host. These record every
    //! request: where it goes, how often a refusal is asked, and what a 404 costs.

    use super::*;
    use aterm_update_core::{HeadAnswer, HttpError};

    const SLUG: &str = "alabsystems/aterm";
    const TAG: &str = "v0.92.0";

    fn served() -> std::result::Result<HeadAnswer, HttpError> {
        Ok(HeadAnswer {
            code: 302,
            location: Some(
                "https://release-assets.githubusercontent.com/github-production-release-asset/x"
                    .into(),
            ),
        })
    }

    fn status(code: u16) -> std::result::Result<HeadAnswer, HttpError> {
        Ok(HeadAnswer {
            code,
            location: None,
        })
    }

    fn names() -> Vec<String> {
        channel::required_asset_names("0.92.0", true, true)
    }

    /// Run the proof over a scripted transport; returns (result, every URL asked,
    /// every sleep taken).
    fn run(
        names: &[String],
        mut answer: impl FnMut(&str, usize) -> std::result::Result<HeadAnswer, HttpError>,
    ) -> (Result<u32>, Vec<String>, Vec<Duration>) {
        let mut asked = Vec::new();
        let mut slept = Vec::new();
        let out = prove_assets_download_anonymously(
            SLUG,
            TAG,
            names,
            &mut |url| {
                asked.push(url.to_string());
                let nth = asked.iter().filter(|u| *u == url).count();
                answer(url, nth)
            },
            &mut |d| slept.push(d),
        );
        (out, asked, slept)
    }

    /// The recording stub: every request the channel readability proof makes is a HEAD
    /// of `github.com/<slug>/releases/download/<tag>/<asset>`, one per required asset,
    /// and none is addressed to `api.github.com`.
    #[test]
    fn the_readability_proof_never_asks_the_api_host() {
        let names = names();
        let (out, asked, slept) = run(&names, |_, _| served());
        assert_eq!(out.unwrap(), names.len() as u32);
        assert!(slept.is_empty(), "a served channel needs no second look");
        let want: Vec<String> = names
            .iter()
            .map(|n| format!("https://github.com/{SLUG}/releases/download/{TAG}/{n}"))
            .collect();
        assert_eq!(asked, want, "one HEAD per required asset, nothing else");
        for url in &asked {
            assert!(
                !aterm_update_core::cdn::is_api_host(url) && !url.contains("api.github.com"),
                "the channel's anonymous proof spent the metered API: {url}"
            );
        }
        // Negative control: the predicate flags an API-host URL
        // (`GITHUB_API_ORIGIN/repos/<slug>/releases/tags/<tag>`).
        let retired = format!("{GITHUB_API_ORIGIN}/repos/{SLUG}/releases/tags/{TAG}");
        assert!(aterm_update_core::cdn::is_api_host(&retired));
    }

    /// A 403 or a 429 is asked once: no retry, no sleep, and not called a rate limit
    /// (the download host has none).
    #[test]
    fn a_refusal_is_asked_once_and_is_not_called_a_rate_limit() {
        for code in [403_u16, 429] {
            let (out, asked, slept) = run(&names(), |_, _| status(code));
            out.expect_err("a refused channel is not proved readable");
            assert_eq!(
                asked.len(),
                1,
                "HTTP {code} was asked {} times",
                asked.len()
            );
            assert!(
                slept.is_empty(),
                "HTTP {code} slept before refusing: {slept:?}"
            );
        }
    }

    /// A 404 is the CDN converging after the flip: retried on the short budget, and a
    /// later 302 proves the asset. A 404 that never clears is refused after exactly the
    /// budget.
    #[test]
    fn a_404_is_retried_until_the_cdn_converges_and_refused_when_it_never_does() {
        let names = names();
        let first = names[0].clone();
        let (out, asked, slept) = run(&names, |url, nth| {
            if url.ends_with(&format!("/{first}")) && nth <= 2 {
                status(404)
            } else {
                served()
            }
        });
        assert_eq!(out.unwrap(), names.len() as u32 + 2);
        assert_eq!(asked.len(), names.len() + 2);
        assert_eq!(slept, vec![ANON_PROBE_DELAY; 2]);

        let (out, asked, slept) = run(&names, |_, _| status(404));
        out.expect_err("a channel that never serves is refused");
        assert_eq!(asked.len(), ANON_PROBE_ATTEMPTS as usize);
        assert_eq!(slept.len(), ANON_PROBE_ATTEMPTS as usize - 1);
    }

    /// The classification table, including what is not readable: a 200 (GitHub never
    /// serves the asset itself — a captive portal does), a redirect off the asset
    /// storage, a plain-http or userinfo redirect, a permanent redirect. The storage is
    /// the `githubusercontent.com` domain, so a host GitHub has not used yet reads as
    /// downloadable too — a storage move must not refuse a cut.
    #[test]
    fn only_a_redirect_into_the_asset_storage_reads_as_downloadable() {
        let at = |code: u16, location: &str| {
            classify_asset_head(Ok(HeadAnswer {
                code,
                location: Some(location.to_string()),
            }))
        };
        assert_eq!(
            at(302, "https://release-assets.githubusercontent.com/x"),
            AssetHead::Downloads
        );
        assert_eq!(
            at(307, "https://objects.githubusercontent.com/x"),
            AssetHead::Downloads
        );
        assert_eq!(
            at(302, "https://release-objects.githubusercontent.com/x"),
            AssetHead::Downloads
        );
        for (code, location) in [
            (301, "https://release-assets.githubusercontent.com/x"),
            (302, "http://release-assets.githubusercontent.com/x"),
            (
                302,
                "https://release-assets.githubusercontent.com.evil.example/x",
            ),
            (302, "https://user@release-assets.githubusercontent.com/x"),
            (302, "https://githubusercontent.com/x"),
            (302, "https://portal.example/login"),
        ] {
            assert!(
                matches!(at(code, location), AssetHead::Inconclusive(_)),
                "{code} {location} must not read as downloadable"
            );
        }
        assert!(matches!(
            classify_asset_head(status(200)),
            AssetHead::Inconclusive(_)
        ));
        assert_eq!(classify_asset_head(status(404)), AssetHead::NotServed);
        assert_eq!(classify_asset_head(status(403)), AssetHead::Refused(403));
        assert_eq!(classify_asset_head(status(429)), AssetHead::Refused(429));
        assert!(matches!(
            classify_asset_head(Err(HttpError::Transport("curl: (6) DNS".into()))),
            AssetHead::Inconclusive(_)
        ));
    }

    /// The curl-stderr half, which the pointer gate's appcast GET still uses: a 403 or
    /// a 429 is a refusal (asked once), anything else is retried weather.
    #[test]
    fn the_curl_refusal_reader_names_403_and_429_only() {
        assert_eq!(
            anon_probe_refusal("curl: (22) The requested URL returned error: 403"),
            Some(403)
        );
        assert_eq!(
            anon_probe_refusal("curl: (22) The requested URL returned error: 429"),
            Some(429)
        );
        assert_eq!(
            anon_probe_refusal("curl: (22) The requested URL returned error: 404"),
            None
        );
        assert_eq!(
            anon_probe_refusal("curl: (56) Recv failure: Connection reset by peer"),
            None
        );
        assert_eq!(anon_probe_refusal(""), None);
    }
}

#[cfg(test)]
mod remedy_tests {
    use super::*;

    /// A fresh cut over an in-flight journal is refused, real or dry.
    #[test]
    fn an_in_flight_cut_refuses_a_fresh_cut() {
        let journal = JournalHeader {
            format: JOURNAL_FORMAT,
            version: "0.92.0".into(),
            build_number: 1_790_200_000,
            commit: "a".repeat(40),
            done: vec!["lock".into()],
        };
        for kind in [CutKind::Real, CutKind::DryRun] {
            assert!(fresh_cut_journal_triage(Some(&journal), kind).is_err());
        }
    }
}

#[cfg(test)]
mod artifact_proof_tests {
    //! One behavioural pass per cut: `publish` re-proves `dist/` by digest
    //! ([`prove_publication_on_disk`]) and runs nothing; only `selfcheck` runs the
    //! shipped binary, its signed session and `codesign`.

    use super::*;

    const VERSION: &str = "0.5.0";
    const BUILD: u64 = 500;

    /// A `dist/` holding everything a finished `sign` leaves (plist, provenance, DMG,
    /// zip and the manifest naming their digests) but an app bundle with no executable:
    /// a proof that spawns the binary cannot pass here, and one that only reads bytes
    /// must.
    fn fixture(label: &str) -> (aterm_tempfile::TempDir, CutCtx) {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-artifact-proof-{label}-"))
            .tempdir()
            .unwrap();
        let root = dir.path().to_path_buf();
        let dist = root.join("dist");
        let app = bundle::staged_app_path(&dist, BUILD);
        fs::create_dir_all(app.join("Contents")).unwrap();
        fs::write(
            app.join("Contents/Info.plist"),
            format!(
                "<plist><dict><key>CFBundleShortVersionString</key><string>{VERSION}</string>\
                 <key>CFBundleVersion</key><string>{BUILD}</string></dict></plist>"
            ),
        )
        .unwrap();
        let commit = "a".repeat(40);
        fs::write(
            dist.join(format!("aterm-{VERSION}-build.txt")),
            format!(
                "version={VERSION}\nbuild={BUILD}\ncommit={}\n",
                &commit[..12]
            ),
        )
        .unwrap();
        let dmg = dist.join(channel::dmg_asset_name(VERSION));
        let zip = dist.join(channel::zip_asset_name(VERSION));
        fs::write(&dmg, b"the lean dmg bytes").unwrap();
        fs::write(&zip, b"the updater zip bytes").unwrap();
        let dmg_sha = dmg::sha256_file(&dmg).unwrap();
        let zip_sha = dmg::sha256_file(&zip).unwrap();
        stage_manifest(
            &dist,
            &manifest_out::ManifestInputs {
                version: VERSION,
                build_number: BUILD,
                commit: &commit,
                dmg_name: &channel::dmg_asset_name(VERSION),
                dmg_sha256: &dmg_sha,
                zip_name: &channel::zip_asset_name(VERSION),
                zip_sha256: &zip_sha,
                repo_slug: "channel/repo",
                min_os: "11.0",
                team_id: "",
                pub_date: "2026-09-23T00:00:00Z",
                min_build: None,
                changelog: "- a change",
            },
            None,
            None,
        )
        .unwrap();
        let ctx = CutCtx {
            credentials: None,
            apple: sign::AppleTier::Inactive,
            repo: root.clone(),
            tree: root,
            dist: dist.clone(),
            journal_path: dist.join("cut-state.toml"),
            slug: "owner/repo".to_string(),
            version: VERSION.to_string(),
            tag: format!("v{VERSION}"),
            build: BUILD,
            commit,
            min_build: None,
            artifact: Some("aa".repeat(32)),
            macho_sha256: Some("bb".repeat(32)),
            manifest_signed: false,
            signature_required: false,
            signature_pubkey: None,
            verify_pubkey: None,
            signature_machine_id: None,
            attribution: None,
            roster: None,
            release_id: None,
            release_intent: false,
            upload_intents: Vec::new(),
            kind: CutKind::DryRun,
            lease: None,
            fence: None,
            notes_section: VERSION.to_string(),
            journal: None,
            linux: None,
        };
        (dir, ctx)
    }

    #[test]
    fn the_publication_steps_reprove_bytes_and_run_nothing() {
        let (_fixture, mut ctx) = fixture("bytes");
        let manifest = prove_artifacts_on_disk(&ctx).expect("the bytes are the manifest's");
        assert_eq!(manifest.build_number, BUILD);
        // The twins and sidecars a resume may lack are regenerated from the proven
        // digests.
        assert!(ctx.stable_dmg_path().is_file() && ctx.dmg_sha256_path().is_file());

        // Negative control: the self-check proper runs the shipped binary, so with none
        // to run it refuses.
        let error = step_selfcheck(&mut ctx)
            .expect_err("the behavioural pass must run the binary")
            .to_string();
        assert!(error.contains("spawn aterm --diagnose"), "{error}");
    }

    #[test]
    fn a_byte_that_changes_after_the_selfcheck_is_refused_by_digest() {
        let (_fixture, ctx) = fixture("dmg-flip");
        prove_artifacts_on_disk(&ctx).expect("the untouched bytes pass");
        let mut dmg = fs::read(ctx.dmg_path()).unwrap();
        dmg[0] ^= 1;
        fs::write(ctx.dmg_path(), &dmg).unwrap();
        let error = prove_artifacts_on_disk(&ctx)
            .expect_err("a flipped DMG byte must be refused")
            .to_string();
        assert!(error.contains("DMG sha256"), "{error}");

        let (_fixture, ctx) = fixture("zip-flip");
        let mut zip = fs::read(ctx.zip_path()).unwrap();
        zip[0] ^= 1;
        fs::write(ctx.zip_path(), &zip).unwrap();
        let error = prove_artifacts_on_disk(&ctx)
            .expect_err("a flipped zip byte must be refused")
            .to_string();
        assert!(error.contains("zip sha256"), "{error}");
    }
}
