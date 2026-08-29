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

use aterm_digest::Sha256;
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};

use aterm_update_core::Manifest;
use aterm_update_core::roster;
use aterm_update_core::tag::TagError;

use crate::ledger::{self, Error, GitCli, GitRunner, Result, RunOut, git_ok, rev_parse};
use crate::{
    buildplan, bundle, changelog, dmg, gates, machines, manifest_out, mirror, sign, verify,
};

// ---------------------------------------------------------------------------
// CLI-facing surface
// ---------------------------------------------------------------------------

/// Every `cut` flag (spec §5), parsed by cli.rs. (`PartialEq` exists for the
/// CLI parse table in tests/resume.rs.)
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CutOptions {
    /// Path to the ONE credentials profile (`--release-credentials`). A PATH only
    /// here: the loaded material lives in `CutCtx`, and only the derived public
    /// identity is ever journaled.
    pub release_credentials: Option<PathBuf>,
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
    /// `--no-paint-smoke`: skip the self-check's paint smoke (the ten-keystroke
    /// pixel proof against the just-built bundle). An EMERGENCY escape, refused
    /// on a notarized real cut unless [`NO_PAINT_SMOKE_ACK_VAR`] carries the
    /// exact acknowledgement — see [`paint_smoke_policy`].
    pub no_paint_smoke: bool,
    /// `--strand-pre-roster-clients`: the operator asserts that no client running a
    /// build older than the machine roster is left in the field, so this cut may be
    /// signed by a key that is on the roster but in no shipped keyset.
    ///
    /// Meaningless — and inert — while `pins::PAPER_MASTER_PUBKEYS` is empty: with no
    /// master pinned, the keyset IS the authority and a non-member is refused by
    /// `committed_channel_signature_policy` with no flag able to change that. See
    /// [`PreRosterClients`].
    pub strand_pre_roster_clients: bool,
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

/// The column every value starts in: two spaces of indent, the label, then one
/// HARD space. Thirteen is what the transcript already rendered, so nothing moves.
///
/// The hard space is the whole point. The old primitive was `"  {label:<11}"`, and
/// `{:<11}` pads only when the label is SHORTER than 11 — an 11-character label got
/// no separator at all and ran straight into its own value. That is not theoretical:
/// `print_check("seed source", …)` printed `seed source10 program(s) staged at …`
/// in a real provisioning run. A pad that can vanish is not a gutter.
pub const VALUE_COL: usize = 13;

/// The longest label the grid can carry: 2 indent + 10 + 1 hard space = [`VALUE_COL`].
///
/// Enforced by a `debug_assert` in [`grid_block`] and by a census over every label
/// literal in the crate (`tests/transcript_grid.rs`), because the failure mode is a
/// line that still PRINTS — it just prints two facts glued together.
pub const LABEL_MAX: usize = VALUE_COL - 3;

/// How wide a transcript line may be.
///
/// `COLUMNS` when the shell exports it, else 100: wide enough for the transcript's
/// natural line, narrow enough that an 80-column window only wraps the tail. It is
/// deliberately NOT a tty probe — a piped run wraps exactly the way the terminal
/// does, so a transcript pasted into an issue reads like the screen it came from.
/// That matters here more than in most tools: the thing an operator sends for help
/// is `provision 2>&1 | tee`, and a wall of unwrapped 600-column paragraphs is the
/// form in which the warnings that cost a certificate slot went unread.
fn width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(100)
        .clamp(60, 120)
}

/// Break one message into value-column rows.
///
/// The rules, in order, and each one exists because a message needed it:
///
/// 1. **Segment on `'\n'` first.** An author-supplied newline is honoured absolutely.
///    It is how a block gets a verdict line, then an act line, then bullets — the
///    shape that survives skimming.
/// 2. **A segment's own leading spaces become ITS hanging indent**, so a sub-bullet
///    or an indented path stays visually attached to the line above it instead of
///    unwrapping flush against unrelated prose.
/// 3. **Break on `' '` only.** A token longer than the remaining budget goes out
///    WHOLE and is allowed to overrun. Paths, base64 public keys, URLs and commands
///    stay one unbroken double-clickable token; a hyphenated public key is a public
///    key the operator cannot paste.
/// 4. **An empty segment is a genuinely empty line** — no gutter, no trailing spaces.
/// 5. **A trailing space survives**, because a prompt ends `"… [y/N] "` and the
///    cursor has to sit one space clear of the question.
fn wrapped(msg: &str, width: usize) -> Vec<String> {
    let budget = width.saturating_sub(VALUE_COL);
    let mut out: Vec<String> = Vec::new();
    for seg in msg.split('\n') {
        if seg.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        // A segment's leading spaces are its hanging indent — and so is a LIST MARKER.
        // `· it does NOT fall back…` wrapping flush under the bullet turns a five-item
        // warning into a paragraph with dots in it; hanging the continuation under the
        // item's TEXT is what keeps the items countable at a glance, which is the whole
        // reason it is a list.
        let lead = seg.len() - seg.trim_start_matches(' ').len();
        let body = &seg[lead..];
        let marker = &body[..bullet_marker(body)];
        let hang = " ".repeat(lead + marker.chars().count());
        let mut line = String::from(&seg[..lead]);
        line.push_str(marker);
        let mut filled = false;
        let mut rest = &body[marker.len()..];
        while !rest.is_empty() {
            // The run of spaces BEFORE this word is kept, not collapsed. A double space
            // is the transcript's mini-column separator — `key  <path>  0600, stays on
            // this machine  (pub …)` is three fields, and a wrapper that normalises
            // whitespace turns them into one sentence.
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

/// The BYTE length of a leading list marker — `· `, `- `, `* `, `1. ` — or 0.
///
/// Bytes, because it is used to split the segment; the marker's COLUMN width is taken
/// separately with `chars().count()`, and for `· ` those two differ (3 bytes, 2 columns).
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

/// Exactly what [`step`] prints, as a `String` with NO trailing newline.
///
/// It is a separate function so a PROMPT can be written to `/dev/tty` in the
/// transcript's own grid. There used to be three hand-rolled gutters — a
/// `" ".repeat(13)`, a `{:<11}`, and a hand-counted `"\n  notary   "` that was
/// three columns shy — printing questions about permanent, irreversible acts in a
/// layout that did not match the lines around them. A question that arrives under a
/// different gutter reads as a different subject.
pub fn grid_block(label: &str, msg: &str) -> String {
    grid_block_at(width(), label, msg)
}

/// [`grid_block`] at an EXPLICIT width, so a test can assert what an 80-column window
/// shows without mutating `COLUMNS` out from under every other test in the process.
pub fn grid_block_at(width: usize, label: &str, msg: &str) -> String {
    debug_assert!(
        label.chars().count() <= LABEL_MAX,
        "label {label:?} is {} columns; the grid carries {LABEL_MAX} \
         (a longer one eats its own separator — see VALUE_COL)",
        label.chars().count()
    );
    // Nothing to say prints NOTHING — not a labelled empty row, and not thirteen
    // spaces of invisible gutter. `step("signing", "")` used to render as the word
    // `signing` followed by emptiness, twice, bracketing the loudest warning the
    // tool can print; at a labelled empty row the operator's first thought is that
    // output was lost, at exactly the moment they are being told they are about to
    // wedge an installed base forever.
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
/// every continuation aligned under the value. Continuation lines pass `""`.
///
/// Call sites never pad, never count columns and never hand-break a line: the width
/// is decided here, once, so a message widens with the terminal instead of being
/// frozen at whatever fitted the author's window.
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
pub(crate) fn channel_token_path() -> Option<PathBuf> {
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

/// Run `body` with the public-channel credential in force (RAII, cleared on every
/// exit path). For callers outside this module that must talk to the channel —
/// `verify::run_retire_unmirrored` deleting an orphaned public draft — so the call
/// is authenticated as the release org, not the dev account (which cannot even SEE
/// a draft on the channel: GitHub answers 404, and the caller would "succeed" by
/// doing nothing).
pub(crate) fn with_channel_cred<T>(body: impl FnOnce() -> Result<T>) -> Result<T> {
    let _cred = ChannelCred::enter();
    body()
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

/// The operator's assertion that NO draft was ever posted for this tag, for the one
/// recovery state nothing else can answer.
///
/// A publisher that died with its journal takes `create_intent_knowledge` to `None`,
/// and an ABSENT release object then means one of two things a machine cannot tell
/// apart: no create POST was ever issued, or one was issued and has not become
/// visible yet. Refusing (the safe reading) left no command that could release
/// `refs/tags/aterm-release-lease`, so every later `cargo ship cut` refused on every
/// machine and the refs had to be deleted by hand — the pipeline stayed wedged by a
/// safety rule protecting against a draft that did not exist.
///
/// Only a human can close that gap, by looking at the releases page. This flag is
/// that answer, and it is deliberately SEPARATE from
/// [`RECOVERY_STOPPED_PROCESS_FLAG`] — which is mandatory and asserts something else
/// entirely — so the weaker claim is never made silently as a side effect of the
/// stronger one. It relaxes nothing when the journal actually knows: a journal that
/// PROVES a POST was issued still wins, because delayed visibility is then the only
/// explanation and waiting is correct.
pub const RECOVERY_NO_DRAFT_POSTED_FLAG: &str = "--no-draft-was-posted";
pub const RECOVERY_STOPPED_PROCESS_REFUSAL: &str = "lost-machine recovery requires explicit proof that the old publisher process is stopped; \
     a fence rotation cannot cancel an already in-flight GitHub REST request";
pub const RECOVERY_STOPPED_PROCESS_BANNER: &str =
    "OPERATOR ASSERTION: old publisher is stopped; Git fencing cannot cancel in-flight REST";

/// Mandatory acknowledgement for the OTHER operation whose safety has an external,
/// operator-established precondition: cutting under a key that only ROSTER-AWARE
/// clients can verify.
///
/// Same shape and same reasoning as [`RECOVERY_STOPPED_PROCESS_FLAG`] — the program
/// cannot prove that no pre-roster client is left in the field, and it is not going to
/// pretend it can. See [`PreRosterClients`] for why this is a flag on the command
/// rather than a key in the credentials profile.
pub const PRE_ROSTER_STRANDING_FLAG: &str = "--strand-pre-roster-clients";

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
///
/// `site` (format 8) runs AFTER `unlock`, deliberately: it touches no release
/// object — it re-runs `publish/post-promote --latest` so alab.systems' download
/// button names the DMG this cut just mirrored — so the release must already be
/// live, mirrored, and lease-free before it starts, and a website failure parks
/// the journal at `site` (loud, `--resume`-able) while the RELEASE itself is
/// complete and untouched by any retry.
pub const STEPS: [&str; 13] = [
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
    "site",
];

/// Steps that run after the release lease was CAS-deleted by `unlock`. An
/// entry (fresh or resumed) whose next step is one of these must not acquire
/// or demand the lease/fence: the release is already live, verified, mirrored
/// and unlocked, and re-acquiring would mint a lock nothing will ever delete.
const POST_UNLOCK_STEPS: [&str; 1] = ["site"];

pub fn is_post_unlock_step(step: &str) -> bool {
    POST_UNLOCK_STEPS.contains(&step)
}

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

/// Format-7 step order — identical to [`STEPS`] minus the post-unlock website
/// `site` step, which format 8 appended after `unlock`. Frozen for the same
/// reason as [`STEPS_V5`]/[`STEPS_V6`]: a COMPLETED v7 journal (every cut
/// through v0.63.0) must still read back as complete — walking one against the
/// current list would misfile a finished cut as "resumable at site" and block
/// the next cut behind a step that was never owed.
const STEPS_V7: [&str; 12] = [
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

pub const JOURNAL_FORMAT: u32 = 8;

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
    /// The key the PUBLISHED artifacts must verify UNDER, when that is not this
    /// machine's own signing key.
    ///
    /// `signature_pubkey` was doing both jobs, and for an ordinary cut they are the
    /// same value, so nothing showed. They come apart in exactly the case the
    /// plural-publisher design exists for: machine A publishes a release signed with
    /// Ka and dies; machine B recovers it. B's `signature_pubkey` is Kb — correctly,
    /// because that is what its local guards compare against — and `archive`/`verify`
    /// then tried to verify A's manifest under B's key and failed. The release was
    /// live but unmirrored, `unlock` was never reached, the lease stayed held by the
    /// dead publisher, and no supported command could free it: `--abandon` refuses a
    /// published release, `--retire-unmirrored` wants the mirror step, `yank` wants a
    /// finished journal (2026-08-19 round-6 audit).
    ///
    /// `None` means "the same key this machine signs with", which is every journal a
    /// normal cut writes. Set only by a recovery, and only from the RELEASE's own
    /// master-roster-proven key.
    #[serde(default)]
    pub verify_pubkey: Option<String>,
    /// WHICH MACHINE signed, when the machine-roster tier is armed — the id the
    /// master-signed roster maps [`Self::signature_pubkey`] to. `None` with an
    /// unpinned paper master, which is every journal this tree writes.
    ///
    /// It rides beside the public key rather than replacing it because the two
    /// answer different questions on resume: the key is what the published
    /// signature must verify under, the id is what the published manifest CLAIMS.
    /// A resume that re-authorizes to a DIFFERENT machine must abort, and this is
    /// the recorded value that makes the comparison possible — without it a second
    /// machine could finish the first machine's cut, and the manifest already
    /// carries (and is signed over) the first machine's id, so the release would
    /// ship an attribution its own signer contradicts.
    ///
    /// Public identity only, exactly like [`Self::signature_pubkey`]: nothing
    /// secret is ever journaled.
    #[serde(default)]
    pub signature_machine_id: Option<String>,
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
    // RETIRED 2026-08-26: `lite_dmg_sha256`, the byte authority for the
    // `aterm-<v>-lite.dmg` twin and its `aterm.dmg` alias. The key is simply
    // absent from the struct now; a journal written by the previous cutter
    // that still carries it loads (serde ignores unknown keys), and its
    // mirrored set is judged by today's exact set — a channel head that
    // already received the lean twin is refused at the mirror's exact-set
    // gate ("unexpected aterm-<v>-lite.dmg") for a human to inspect.
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
        let journal: Journal = aterm_toml::from_str(&text)
            .map_err(|e| Error::new(format!("parse {}: {e}", path.display())))?;
        journal.validate()?;
        Ok(Some(journal))
    }

    /// Persist atomically (temp + rename): a crash mid-write must never leave
    /// a torn journal that blocks its own recovery path.
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let text = aterm_toml::to_string(self)
            .map_err(|e| Error::new(format!("serialize journal: {e}")))?;
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
            7 => &STEPS_V7,
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
/// lost/legacy journal and is deliberately as unsafe as a known issued intent —
/// unless the operator answers for it with [`RECOVERY_NO_DRAFT_POSTED_FLAG`], the
/// only way out of a wedge no machine can reason its way through (see that
/// constant). `Some(true)` is never overridable: there, delayed visibility is the
/// only explanation left and waiting is the correct behaviour.
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

/// `claim_bound_draft` means the visible draft under this tag targets the very commit
/// the recovery claim names — the remote's own answer to "is this object mine?".
///
/// # A LOST JOURNAL IS NOT AN UNKNOWN OBJECT
///
/// `None` intent is correctly as unsafe as `Some(true)` when nothing else can speak
/// for the object. But a recovery on a second machine always has `None` (the journal
/// died with the publisher), and refusing on that alone made the pipeline
/// unrecoverable rather than merely careful: a publisher that died between
/// `step_upload` and `step_flip` left a draft no machine could clean and
/// `refs/tags/aterm-release-lease` held by a process that no longer exists. Every
/// later `cargo ship cut` refused at `preflight_release_lease`, `--abandon` refused
/// for want of the same journal, and the only remedy was deleting refs by hand.
///
/// When the remote binds the draft to this claim's commit, the missing local intent
/// adds nothing: that binding is the same one `validate_release_object_capability`
/// enforces before the delete, and `recover` already requires the operator to have
/// proven the old publisher exited ([`RECOVERY_STOPPED_PROCESS_FLAG`]). An unbound
/// draft — someone else's object sitting on this tag — still refuses.
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

/// curl's exit for "I could not even start": argument and initialisation failures,
/// which happen strictly before any connection is attempted.
const CURL_EXIT_FAILED_INIT: i32 = 2;

/// Did this attempt PROVABLY not reach the network?
///
/// The whole one-shot POST design turns on a question it cannot normally answer —
/// "did the server see my request?" — and answers it conservatively: assume yes,
/// never repeat. This is the one case where the answer is knowable locally. curl
/// exits 2 when it rejects its own arguments or fails to initialise, which is
/// before connect(2); nothing was sent, so nothing can have been received, and the
/// conservative assumption is simply false.
///
/// Narrow on purpose. A timeout, a reset, a 5xx, a killed process — none of those
/// qualify, because each can hide a delivered request. Only the local refusal does.
const fn transport_never_started(out: &RunOut) -> bool {
    out.status == CURL_EXIT_FAILED_INIT
}

/// How curl is told to find the request body — the ONE decision that separates a
/// request whose memory cost is its payload from one whose cost is constant.
/// See [`OneShotPost::prepare_binary`] for the gigabyte that made it matter.
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
    /// STREAMED FROM DISK, never buffered. `--data-binary @file` reads the whole
    /// payload into memory before it opens the socket, and the batteries-included
    /// DMG is over a gigabyte: the first seeded cut died here with
    /// `curl: option --data-binary: out of memory`, after building, signing and
    /// notarizing both containers. `--upload-file` streams the same bytes with a
    /// `Content-Length` taken from the file's size, so the transport cost is
    /// independent of the asset (2026-08-19). `--request POST` still fixes the
    /// method — `--upload-file` would otherwise PUT — and the endpoint carries a
    /// query string rather than a trailing `/`, so curl appends no file name of
    /// its own.
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

    /// Takes the header ARGUMENT, not the `GithubAuthHeaders` that owns it: the
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

/// THE ROSTER RATCHET, and the exact sibling of [`channel_floor_covered`]: a cut may
/// not publish a roster generation OLDER than the one already on the channel head.
///
/// # Why the producer needs its own floor at all
///
/// The client keeps a permanent high-water mark. `Floor::bump_and_write` ratchets
/// `roster_seq` on OBSERVATION — whether or not the release was staged — and
/// `Roster::admit` returns `Rollback` for anything below it, before any artifact
/// crypto. `machines::authorize_cut` cannot see that mark (it is remote channel
/// state, not a property of a local file) and deliberately does not pretend to, so
/// without this function the producer's gate is strictly WEAKER than the client's on
/// a channel-visible monotonic counter.
///
/// # Why that gap is the normal case, not the exotic one
///
/// `atpkg-keys`' `DEFAULT_ROSTER` is `dist/aterm-machines.toml` and `/dist/` is
/// gitignored, so the roster is not distributed with the repo: every machine that did
/// not run the mint holds a hand-copied roster, and holding a stale-but-unexpired one
/// is the steady state. Machine B mints or revokes, publishing `roster_seq` 5; every
/// live client ratchets its floor to 5. Machine A, still on its seq-4 copy, passes
/// freshness, passes the deny-list, and publishes — and every client that saw B's
/// release refuses A's with `Rollback`. `select_authoritative_release` picks exactly
/// one candidate with no fallback to an older release, so those clients do not get a
/// later update, they get NO update, and the cut reports success.
///
/// The corollary runs the other way too, and this is what makes the check a security
/// property and not just a hygiene one: a machine revoked at seq 5 is still authorized
/// by its own seq-4 copy. The producer-side deny-list is only ever as current as the
/// least-updated cutter, and a floor read from the channel is what forces it forward.
///
/// # Shape
///
/// `None` on either side means "no roster in play", which is every cut this tree makes
/// and must therefore be `Ok`. An unattributed cut against a rostered head is NOT
/// silently allowed: dropping the tier is a downgrade the client would refuse
/// structurally, so it is named here while naming it is free.
pub fn roster_floor_covered(carried: Option<u64>, newest_channel: Option<u64>) -> Result<()> {
    // A LOWER floor than the client's, deliberately, and by exactly one generation:
    // the client ratchets on OBSERVATION, so its floor is the head's `roster_seq`, and
    // republishing AT that generation is exactly what a second machine holding the same
    // roster does. `>=` admits that and refuses only a genuine step backwards.
    match (carried, newest_channel) {
        (_, None) => Ok(()),
        (Some(carried), Some(newest)) if carried >= newest => Ok(()),
        // Headline, the two numbers that DIAGNOSE the machine set side by side where
        // they can be compared, then the fix, then the mechanism that justifies it.
        // The remedy used to be one clause at the end of an 82-word paragraph whose first
        // sixty words were `RosterReject::Rollback` internals — and the two generations
        // were embedded in prose, which is the one place two numbers cannot be compared.
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
    let raw = aterm_codec::base64::decode_strict(canonical.as_bytes())
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

/// What a pipeline entry will still DO with the signing material, and therefore how
/// much of the roster chain it has any business re-proving.
///
/// The distinction exists because the roster answers a question that stops being
/// askable once the bytes exist. A cut that will still assemble, stamp and SIGN a
/// manifest is choosing an attribution, so it must prove the roster still authorizes
/// this machine. A cut that is finishing already-signed bytes has no such choice
/// left: the attribution is inside a signature, the roster document is frozen in
/// `dist/`, and re-reading today's roster file says nothing about either.
///
/// Treating the second case like the first is not a harmless extra check, it is a
/// wrong one, and it fails in the direction that costs the most:
///
/// * It can only ever fail SPURIOUSLY. Satisfying it — re-signing the roster from the
///   paper master — does not change one byte of what the cut will publish, because
///   `step_mirror` serves the `dist/` bytes that `verify` proved live. So the gate
///   blocks on a condition whose remedy fixes nothing.
/// * It fires on the path taken when something has ALREADY gone wrong. A roster whose
///   window lapses between `flip` and `mirror` would otherwise make a cut that is one
///   upload from done into one that can never be finished, leaving the release live
///   on the publish repo and absent from the public channel the fleet actually reads,
///   with the lease still held.
/// * It refuses cross-machine recovery outright — the one path designed for a dead
///   publisher, in the one design where publishers are plural.
///
/// This is the same trade `resume_apple_tier` makes for an expired certificate, for
/// the same reason, and `resume_cut` already stated it in a comment; [`RosterDuty`] is
/// what makes the statement true at every entry rather than one of them.
///
/// What [`RosterDuty::Finish`] does NOT relax: the committed channel keyset. A key
/// that is not a keyset member could never have produced these bytes, so that check
/// costs nothing and stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterDuty {
    /// This entry may still produce and sign new manifest bytes. The full chain runs.
    Sign,
    /// Every byte this entry will publish is already assembled and signed. The key
    /// decision runs; the roster chain does not.
    Finish,
}

/// WHO THIS CUT WOULD STRAND, and whether anybody said that was acceptable.
///
/// A client that predates the roster verifies the appcast under its own compiled-in
/// `UPDATE_CHANNEL_PUBKEYS` and has never heard of a machine roster. It cannot be
/// taught a new key by anything except a release it already accepts, and
/// `select_authoritative_release` gives it exactly ONE candidate with no fallback to
/// an older release — so a release signed by a key that client does not hold does not
/// delay it, it WEDGES it permanently.
///
/// That is a fact about the FLEET, not about this machine or this roster, and no
/// signed document can answer it: only the operator knows whether any meaningful part
/// of the fleet is still on a pre-roster build. So the cutter refuses by default and
/// takes the answer from the command that ran — the same shape, and for the same
/// reason, as [`RECOVERY_STOPPED_PROCESS_FLAG`]. It is deliberately NOT a key in the
/// release-credentials profile: a profile can only ever NARROW what is accepted
/// (`sign.rs` leans on that property), this WIDENS it, and a file written once would
/// go on answering "yes, strand them" long after the operator stopped meaning it.
///
/// # ⚠ THE TEST IS HEAD EQUALITY, NOT KEYSET MEMBERSHIP — and the difference bricks fleets
///
/// The question is "can a SHIPPED build verify this?", and the only evidence this tree
/// has is `pins::UPDATE_CHANNEL_PUBKEYS` — which is what the NEXT build will carry, not
/// what the fielded ones do. Membership in it is therefore not the property being asked
/// about, and the gap is not theoretical: K2 (`aterm-update-v3`) was appended to that
/// keyset on 2026-08-12 and appears in no published tag at all, exactly as step 1 of the
/// documented rotation requires. A membership test would call K2 "safe for pre-roster
/// clients" while every client in the field holds `[K1]` alone and would wedge on it —
/// and it would do so silently, with no flag and no warning, which is the precise
/// outcome this type exists to prevent.
///
/// Index 0 is the only member the tree can honestly claim the field holds, because
/// promotion TO index 0 is step 3 of that rotation: the reviewed commit in which the
/// operator asserts the adoption window has closed. Every other member is either an
/// incoming key no shipped build carries yet or an outgoing key inside its retirement
/// window; neither is provably held by every pre-roster client. So the test here is
/// equality with the head — the same rule the unarmed path enforces
/// ([`committed_channel_signature_policy`]) — and arming the master consequently does
/// not widen by one key who may sign without saying so out loud.
///
/// A consequence worth stating, because it is the thing an operator will bump into: an
/// ordinary K1→K2 channel rotation needs no flag. Step 3 PROMOTES K2 to index 0 in a
/// reviewed commit, and a cut after that promotion is a cut by the head. The flag is for
/// the case the rotation does not cover — a rostered machine whose key is not, and is not
/// going to be, the committed head — which is exactly the case the roster tier exists to
/// make possible and the one nothing else in the tree can vouch for.
///
/// # Why not a separate committed list of keys known to have SHIPPED
///
/// It was considered: a third anchor recording which keys are actually in the field, so
/// the gate could consult it instead of inferring from index 0. It is worse in the way
/// that matters. Nothing can PROVE adoption — the fleet does not report in — so such a
/// list would still be an operator assertion, only now one written into a file once and
/// consulted forever, which is precisely the objection this type raises against putting
/// the acknowledgement in the credentials profile. It would also be a third anchor to
/// keep in step with two others, and the failure mode of a stale one is silent. Index 0
/// already carries the assertion, made in a reviewed commit, by the person who is in a
/// position to make it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreRosterClients {
    /// A cut is STARTING here and nobody has said otherwise: a signing key that is not
    /// the committed channel HEAD is refused. The fail-closed default.
    #[default]
    Protected,
    /// A cut is starting here and the operator passed
    /// [`PRE_ROSTER_STRANDING_FLAG`], accepting that clients older than the roster
    /// will never install this release or any release after it.
    Stranded,
    /// NOT THIS ENTRY'S QUESTION. A resume, a recovery or a mirror is continuing a cut
    /// that answered it at pre-claim, under a key it is not permitted to change
    /// (`revalidate_ctx_signature_policy` refuses a changed key outright). Re-asking
    /// could only fail spuriously — and it would fail on the path taken when something
    /// has already gone wrong, turning a cut that is one upload from done into one that
    /// can never be finished. Exactly the trade [`RosterDuty::Finish`] makes.
    Answered,
}

/// Where a signing key stands with respect to the clients that predate the roster —
/// the fact [`PreRosterClients`] then decides what to DO about.
///
/// Separated from the decision because the two are different kinds of statement. This
/// one is derivable from the tree and is not the operator's to override; the other is a
/// judgement about the world that nothing in the tree can make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreRosterStanding<'a> {
    /// The committed keyset is EMPTY, so no shipped build pinned a channel key and no
    /// client is verifying under one. There is nobody in a position to be stranded —
    /// the configuration a fork has, and the one the owner reaches once the rollover is
    /// complete.
    NobodyToStrand,
    /// The key IS `UPDATE_CHANNEL_PUBKEYS[0]`. Every client that accepts anything at
    /// all accepts this, which is the strongest statement the tree can make.
    Head,
    /// The key is in the keyset but NOT at index 0. This is the case a membership test
    /// got wrong, and it is the DANGEROUS one precisely because it looks safe: an
    /// accept-only member is appended so that a FUTURE build can carry it (step 1 of
    /// the rotation), so at the moment it is appended no shipped client holds it at
    /// all. Carries its index so the message can say which, and the head so the remedy
    /// can name the key that would have been safe.
    AcceptOnlyMember { index: usize, head: &'a str },
    /// The key is nowhere in the keyset — a freshly minted machine, which is the
    /// ordinary case the roster tier exists to enable.
    Stranger(&'a str),
}

impl PreRosterStanding<'_> {
    /// `Some(reason)` when a cut under this key cannot be verified by clients that
    /// predate the roster; `None` when it demonstrably can. The reason is a sentence
    /// fragment so the refusal and the warning can share one wording and never drift.
    #[must_use]
    pub fn strands(&self) -> Option<String> {
        match self {
            Self::NobodyToStrand | Self::Head => None,
            Self::AcceptOnlyMember { index, .. } => Some(format!(
                "is UPDATE_CHANNEL_PUBKEYS[{index}], an ACCEPT-ONLY member and not the \
                 head (index 0). Membership in this tree's keyset is not the same thing \
                 as being carried by a SHIPPED build: a non-head member was appended so \
                 that a future build could carry it, and until it is promoted to index 0 \
                 in a reviewed commit — which is the act that asserts the adoption window \
                 has closed — no fielded client is known to hold it"
            )),
            Self::Stranger(_) => Some(String::from(
                "is not a member of the committed channel keyset at all \
                 (aterm-update-core::pins, UPDATE_CHANNEL_PUBKEYS)",
            )),
        }
    }

    /// The head every pre-roster client is known to hold, for the remedy line. `None`
    /// only when the keyset is empty, in which case there is no remedy to offer because
    /// there is no problem — and when this key IS the head, in which case there is no
    /// problem either.
    #[must_use]
    pub fn head(&self) -> Option<&str> {
        match self {
            Self::NobodyToStrand | Self::Head => None,
            Self::AcceptOnlyMember { head, .. } | Self::Stranger(head) => Some(head),
        }
    }
}

/// Locate `material` in the committed keyset, by canonical identity rather than by
/// spelling — the same normalisation the client's verifier applies, so two base64
/// aliases of one key cannot be judged differently here than there.
fn pre_roster_standing<'a>(keyset: &'a [&'a str], material: &str) -> Result<PreRosterStanding<'a>> {
    let Some(head_raw) = keyset.first() else {
        return Ok(PreRosterStanding::NobodyToStrand);
    };
    if canonical_update_pubkey(head_raw)? == material {
        return Ok(PreRosterStanding::Head);
    }
    for (index, candidate) in keyset.iter().enumerate().skip(1) {
        if canonical_update_pubkey(candidate)? == material {
            return Ok(PreRosterStanding::AcceptOnlyMember {
                index,
                head: head_raw,
            });
        }
    }
    Ok(PreRosterStanding::Stranger(head_raw))
}

/// Everything the ARMED machine-roster tier decides on, bundled so the gate takes
/// one parameter rather than five and so a caller cannot supply four of them and
/// forget the fifth. Deliberately the same shape as the client's `RosterPolicy`.
///
/// `master_pubkeys` EMPTY is the whole two-state switch: the tier is absent, and
/// [`channel_signature_policy`] delegates verbatim to
/// [`committed_channel_signature_policy`].
pub struct RosterEvidence<'a> {
    /// The pinned paper master(s) — `pins::PAPER_MASTER_PUBKEYS` in production,
    /// armed in this tree since 2026-08-15 (`atpkg-keys setup --id m3`).
    pub master_pubkeys: &'a [&'a str],
    /// The whole committed channel keyset — `pins::UPDATE_CHANNEL_PUBKEYS`, not
    /// just its head.
    ///
    /// With the master ARMED this is no longer an authorization input: the roster
    /// authorizes, and this keyset can neither grant nor deny (see
    /// [`channel_signature_policy`]). It is read for exactly two things, neither of
    /// them a grant: whether it is EMPTY (then no client is verifying under a channel
    /// key at all, so there is nobody to strand), and what its HEAD is (the one member
    /// a shipped build is known to carry — see [`PreRosterClients`] for why a
    /// membership test would be a fleet-bricking mistake here).
    ///
    /// The whole slice rather than the head alone, because "empty" is a fact about the
    /// slice and because a non-head member has to be RECOGNISED to be reported as one:
    /// "that key is accept-only and in no shipped build" is a very different sentence
    /// from "that key is a stranger", and an operator acts differently on each.
    pub committed_keyset: &'a [&'a str],
    /// Whether stranding pre-roster clients is this entry's question, and if so
    /// whether the operator has accepted it.
    pub pre_roster: PreRosterClients,
    /// The master-signed roster this cut claims authority from, read once,
    /// pre-claim. `None` means the profile named none — which is a refusal on the
    /// armed path, never a downgrade.
    pub roster: Option<&'a machines::RosterDocument>,
    /// What this machine claims its id is (profile, else `~/.aterm/machine.toml`).
    /// A cross-check only; see [`machines::declared_machine_id`].
    pub declared_machine_id: Option<&'a str>,
    /// Injected wall clock, so every freshness case is testable without waiting.
    pub now_unix: i64,
    /// Whether this entry can still SIGN, which is what decides whether the roster
    /// chain is any of its business. See [`RosterDuty`].
    pub duty: RosterDuty,
}

/// THE two-state signing gate: the committed channel pin alone, or the channel pin
/// AND the master-signed machine roster.
///
/// # Anchor empty — today's behaviour, byte for byte
///
/// With no paper master pinned this is [`committed_channel_signature_policy`] and
/// nothing else: same verdict, same errors, same absent attribution, so the emitted
/// manifest bytes are identical to every manifest this cutter has ever produced.
/// That is not politeness, it is the bridge:
/// `aterm_update::github::select_authoritative_release` picks exactly ONE candidate
/// (the highest tag) and has no fallback to an older release, so a shipped client
/// that meets a release it cannot verify is not delayed — it is WEDGED there
/// permanently. Any behaviour change on this path is a fleet-bricking bug, which is
/// why the empty-anchor path is a delegation rather than a re-implementation.
///
/// # Anchor armed — the ROSTER governs, and it governs alone
///
/// `aterm_update::github::fetch_authoritative_release` under an armed anchor consults
/// the master-signed roster and nothing else: the compiled-in keyset can no longer
/// refuse what the roster authorized. So this gate does not require keyset membership
/// either — requiring it is what made adding a machine need a shipped release, which
/// is precisely the ceremony the roster exists to remove.
///
/// The keyset is not dead, though, and pretending it is would brick a fleet. It is
/// the allowance held by clients that PREDATE the roster, and those clients are the
/// one party the producer still owes something to: they verify under their own
/// compiled-in keyset, they cannot be taught a new key except by a release they
/// already accept, and release selection gives them no fallback. A cut signed by a
/// key those clients do not hold therefore wedges every one of them, permanently.
///
/// **Which key do they hold? `UPDATE_CHANNEL_PUBKEYS[0]`, and only that one.** A
/// non-head member is by construction either not shipped yet (step 1 of the rotation
/// appends it precisely so a FUTURE build can carry it) or on its way out. So the
/// obligation is tested as equality with the head, exactly as the unarmed path tests
/// it — arming the master changes WHO MAY SIGN, and must not quietly change WHO CAN
/// VERIFY. [`PreRosterClients`] carries the full argument, including the live example
/// (K2) that a membership test would have waved through.
///
/// Only the operator knows whether any pre-roster client is left, so the obligation is
/// enforced as [`PreRosterClients`]: refuse by default, proceed on an explicit
/// per-cut flag, and say loudly what is being given up. Silence is not available —
/// the failure mode is a fleet that never updates again and never says why.
///
/// An EMPTY committed keyset means there is nobody in that position: no shipped build
/// pinned a channel key, so no client is verifying under one. The obligation check is
/// skipped, and only it.
///
/// Every armed failure is a refusal. There is no arrangement of arguments that
/// returns a policy while the anchor is armed and the roster did not authorize.
pub fn channel_signature_policy(
    committed_pubkey: Option<&str>,
    material_pubkey: Option<&str>,
    evidence: &RosterEvidence<'_>,
) -> Result<(SignaturePolicy, Option<roster::Attribution>)> {
    if evidence.master_pubkeys.is_empty() {
        return Ok((
            committed_channel_signature_policy(committed_pubkey, material_pubkey)?,
            None,
        ));
    }
    let Some(material) = material_pubkey else {
        return Err(Error::new(
            "the paper master (aterm-update-core::pins, PAPER_MASTER_PUBKEYS) is pinned, \
             so every cut must be authorized by the master-signed machine roster — but no \
             signing material was supplied. A keyless machine may not cut for a rostered \
             channel; no ledger claim was made",
        ));
    };
    let material = canonical_update_pubkey(material)?;
    // THE OBLIGATION TO CLIENTS THAT PREDATE THE ROSTER. Not an authorization check —
    // the roster below is the authority, and this can neither grant nor deny on its
    // behalf. It answers a different question: can the clients that have never heard
    // of a roster verify what this cut is about to publish?
    //
    // It runs BEFORE the roster chain deliberately. Both refusals are pre-claim and
    // free, but this one is decidable from two strings, and an operator whose key is
    // outside the keyset needs to hear about the fleet they are about to strand rather
    // than about a roster file they would then go and fix for nothing.
    let standing = pre_roster_standing(evidence.committed_keyset, &material)?;
    if let Some(why) = standing.strands() {
        match evidence.pre_roster {
            PreRosterClients::Protected => {
                // 189 words in one paragraph with no line break was the longest string
                // this tool could print, fired at the moment a cut stops — and it hid an
                // invisible fork between two completely different next moves. An operator
                // scanning for "what do I type" found the word FAILED and then a wall,
                // and the likeliest recovery is to reach for the half-remembered flag,
                // which PERMANENTLY WEDGES installed clients. The crate already knew
                // better: the `Stranded` branch below breaks the same argument into
                // lines, and `gates.rs` uses an indented fix:/or: block for exactly this
                // shape of fork.
                //
                // Every fact is kept — the key, its index, what ACCEPT-ONLY means, what
                // promotion to index 0 asserts, who accepts, who is wedged, both
                // remedies, and the `machine_id` requirement. What changes is that the
                // reassuring fact (no ledger claim) is hoisted out of the tail, where it
                // answers the operator's first worry, and that the fork is a list.
                return Err(Error::new(format!(
                    "publishing under this key would permanently wedge every client older \
                     than the machine roster. No ledger claim was made.\n\
                     \n\
                     the key       {material}\n\
                     \x20             {why}\n\
                     who accepts   every ROSTER-AWARE client — the master-signed roster \
                     authorizes this machine\n\
                     who does not  a client running a build older than the roster: it \
                     verifies the appcast under its own compiled-in keyset, has NO \
                     fallback to an older release, and would never update again\n\
                     \n\
                     CHOOSE ONE\n\
                     \x20 1. cut with the committed channel head {} — the roster names it \
                     as a machine for exactly this reason. The release-credentials profile \
                     on THAT machine must set `machine_id` to the roster id it is listed \
                     under, or the cut refuses there too.\n\
                     \x20 2. pass {PRE_ROSTER_STRANDING_FLAG} — ONLY if no client older \
                     than the roster is left in the field. This is not a delay for them, \
                     it is permanent: a reinstall is the only remedy.",
                    // `strands()` is `Some` only for the two variants that carry a head,
                    // so the fallback is unreachable — spelled out rather than unwrapped
                    // because a refusal path is the worst place to learn that.
                    standing.head().unwrap_or("(the keyset is empty)"),
                )));
            }
            PreRosterClients::Stranded => {
                // Loud, unmissable, and printed on every entry that signs under such a
                // key — not once at the moment the flag was invented.
                // Air, not a labelled empty row. `step("signing", "")` rendered as the
                // word `signing` followed by nothing, twice, bracketing the loudest
                // warning this tool can print — and an operator's first thought at a
                // labelled empty row is that output was lost, at exactly the moment they
                // are being told they are about to wedge an installed base forever.
                println!();
                step(
                    "signing",
                    "⚠ STRANDING PRE-ROSTER CLIENTS, because you asked for it",
                );
                step("", &format!("the signing key {material} {why}"));
                // Same words, one string: the wrapper owns the breaks, so this widens
                // with the terminal instead of staying frozen at the author's window.
                step(
                    "",
                    "every client running a build older than the machine roster verifies \
                     the appcast under its own compiled-in keyset, and release selection \
                     has NO fallback to an older release. Those clients will not install \
                     this release, or any release after it, ever — they are not delayed, \
                     they are wedged, and a reinstall is the only remedy.",
                );
                println!();
            }
            PreRosterClients::Answered => {}
        }
    }
    // A FINISH entry stops here, with the key decision made and no attribution
    // claimed. It has nothing left to sign, so it has no roster question to answer;
    // see [`RosterDuty`] for why asking anyway is a wrong check rather than a spare
    // one. Returning `None` for the attribution is the honest answer and is what stops
    // a caller comparing a fresh local claim against bytes that already shipped.
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
            "the paper master is pinned but the release-credentials profile names no \
             `machine_roster`. An armed anchor never degrades to the single-key path: \
             name the master-signed aterm-machines.toml (its <path>.sig must sit beside \
             it), or unpin the master in a tracked commit",
        ));
    };
    let who = machines::authorize_cut(
        evidence.master_pubkeys,
        document.bytes.clone(),
        &document.signature,
        &material,
        evidence.now_unix,
    )?;
    // The cross-check, last because it is the cheapest and the least authoritative:
    // the roster has already decided who this key belongs to. A profile (or a
    // `~/.aterm/machine.toml`) that disagrees means a copied profile, a re-minted
    // machine, or a mixed-up pair of keys — every one of which would publish an
    // attribution that is true of the bytes and false of the world.
    if let Some(declared) = evidence.declared_machine_id
        && declared != who.machine_id
    {
        // THE REMEDY MUST NOT POINT AT THE CLIFF. There are two ways out of a mismatch —
        // correct the declaration, or change the key — and they are not symmetric. On the
        // bootstrap machine the SAFE path is exactly the one that trips this check
        // (`~/.aterm/machine.toml` says "m3" while the cut has to go out under the
        // incumbent head's key), so an operator following the second suggestion switches
        // to m3's key, lands on the pre-roster refusal, and is handed
        // `--strand-pre-roster-clients` as the way through. Two fail-closed refusals
        // composing into a staircase whose bottom step bricks the installed base is still
        // a bug: it is the program leading the way. So the alternative is offered only
        // when taking it would NOT strand anyone, and named as the hazard it is otherwise.
        let alternative = match machines::roster_pubkey_for(
            evidence.master_pubkeys,
            document.bytes.clone(),
            &document.signature,
            declared,
        )
        .map(|key| canonical_update_pubkey(&key))
        .transpose()?
        .map(|key| pre_roster_standing(evidence.committed_keyset, &key))
        .transpose()?
        {
            Some(standing) if standing.strands().is_some() => format!(
                ". Do NOT switch to {declared:?}'s key to satisfy this: that key cannot be \
                 verified by clients that predate the roster, so it would trade an \
                 attribution mismatch for a permanently wedged installed base"
            ),
            Some(_) => format!(", or cut with the key that belongs to {declared:?}"),
            // The roster does not name the declared machine at all (or the document did
            // not re-verify). No alternative can be recommended, because there is no key
            // to recommend — say nothing rather than guess.
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
/// The key arrives as an ARGUMENT — from the committed pin
/// (`aterm_update_core::pins::update_channel_signing_pubkey`), the release
/// journal, or the machine roster. These messages used to name
/// `ATERM_UPDATE_PUBKEY`, which sent an operator hunting for an environment
/// variable this function has never consulted and that was retired along with
/// the ambient `release.conf` (docs/RELEASING.md).
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
        .map_err(|_| Error::new("ATERM_UPDATE_PUBKEY is too large to re-encode"))
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
            "published signature history activates Tier SIG, but no pinned updater \
             signing key is available (aterm_update_core::pins::UPDATE_CHANNEL_PUBKEYS); \
             verification cannot fall back to unsigned",
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

// `signer_tool` lived here: it searched PATH and target/release for an
// `atpkg-keys` binary to shell out to for manifest signing. It is DELETED rather
// than revived, because reviving it would undo the credentials redesign. Signing
// is in-process now (`load_signing_material` below, docs/RELEASE-KEYS.md: "no
// spawning atpkg-keys... no second binary required to cut"), so the function had
// no caller, and its error text still instructed the operator to fix
// `~/.aterm/release.conf` — a file the same redesign retired. Searching `$PATH`
// for the thing that signs releases is exactly the ambient discovery that
// `--release-credentials` exists to abolish; there is no honest way to make this
// reachable again.

/// The loaded signing identity. There is no longer a `tool` or a `key_path`: the
/// key is held in memory by [`sign::ReleaseCredentials`], loaded once from the path
/// given to `--release-credentials`, and signing happens in-process. The old shape
/// carried a PATH and shelled out to `atpkg-keys pubkey`, which meant a release could
/// not be cut without a second binary built and present.
struct SigningMaterial {
    pubkey: String,
}

/// Derive the signing identity from the credentials supplied on the command line.
///
/// `None` means no `--release-credentials` was given — legal only for an unpinned
/// channel, which `committed_channel_signature_policy` decides, not this function.
/// Nothing here reads the filesystem or the environment: whether a machine can cut
/// is now a property of the command that ran, not of ambient state.
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

/// Resolve Tier APPLE from THE anchor.
///
/// `pins::anchor_active` is the predicate, so an unpinned build is inert by
/// exactly the same rule every other consumer of the anchor uses — the updater,
/// atpkg, and `tools/install.sh` alike.
///
/// The anchor is a PARAMETER, not a read. The two cut entry points ([`run_cut`]
/// and [`resume_cut`]) are the only places that name `pins::APPLE_TEAM_ID` for
/// the purpose of deciding anything (`step_build` also names it, but only to copy
/// it verbatim into the manifest), and they pass it inward from there. That is
/// what makes every decision below this line drivable by a test with a
/// placeholder team: a resolver that read the constant itself would be inert in
/// this tree — the anchor is empty — and its rules would be untestable for
/// exactly as long as they are unused, which is exactly as long as nobody would
/// notice them breaking.
///
/// Note that this deliberately does NOT vary by [`CutKind`]. A dry run or a
/// rehearsal with the anchor set signs and notarizes for real, which costs a
/// submission and several minutes. That is the point: a rehearsal that skips the
/// slowest, most failure-prone, most externally-dependent step in the pipeline
/// rehearses the easy part, and the self-check it then runs would fail anyway —
/// `spctl` rejects a bundle that was never notarized. One path, exercised the
/// same way every time, beats a second path that only runs when it is least
/// wanted.
fn resolve_apple_tier(
    team_id: &str,
    credentials: Option<&sign::ReleaseCredentials>,
) -> Result<sign::AppleTier> {
    if !aterm_update_core::pins::anchor_active(team_id) {
        return Ok(sign::AppleTier::Inactive);
    }
    let tier = sign::resolve_apple_tier(team_id, credentials).map_err(Error::new)?;
    // Announced only when ACTIVE. The inactive tier must add zero steps and zero
    // transcript lines: a cut that signs ad-hoc, as every shipped cut does, looks
    // exactly as it did before Tier APPLE was wired.
    println!("{}", tier.describe());
    Ok(tier)
}

/// The tier a RESUME must resolve — which is nothing at all unless `build` is
/// still going to run.
///
/// A resume is the path taken when something has already gone wrong, and the
/// cost of demanding a credential it will never use is that a cut which is one
/// upload away from finished cannot be finished at all. Only [`step_build`]
/// reads `ctx.apple`; every later step re-proves the artifacts ON DISK against
/// the MANIFEST's `team_id` (see [`selfcheck_signing`]), which is the claim that
/// actually ships and is independent of whatever the keychain holds today. So a
/// resume past `build` needs no certificate, and asking for one only converts a
/// recoverable cut into an unrecoverable one when a certificate expires between
/// the build and the upload.
///
/// The fail-closed property is untouched where it bites: when `build` WILL run,
/// this resolves exactly as [`run_cut`] does, from the same anchor, with the same
/// hard failure if the machine cannot keep the anchor's promise. The gate is
/// deliberately the same predicate as the signing-key re-proof immediately above
/// its call site — "is this resume going to bake artifact bytes?" — so the two
/// credentials a rebuild needs are demanded under one rule rather than two that
/// can drift apart.
pub fn resume_apple_tier(
    team_id: &str,
    journal: &Journal,
    credentials: Option<&sign::ReleaseCredentials>,
) -> Result<sign::AppleTier> {
    if journal.is_done("build") {
        // Not a claim that the tier is off — the build that already ran resolved
        // the real tier, and its artifacts carry whatever it did. It is the
        // statement that nothing REMAINING will sign or notarize, and
        // `AppleTier::Inactive` is precisely "no identity, no auth, every hook a
        // no-op", which is what must happen if one were somehow reached.
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

/// One point-in-time answer to BOTH halves of a download-bracket end.
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

/// The fused bracket read's jq. Each row is tagged with its kind so the two
/// existing parsers keep owning their own row shapes: the fields after `R` are
/// byte-identical to [`RELEASE_IDENTITY_OBJECT_JQ`] and the fields after `A` to
/// the asset-identity program in
/// [`release_asset_identity_for_release_id_optional`]. `.assets[]?` matches the
/// release-scan listing's spelling; on a release with no assets both spellings
/// yield zero rows, i.e. "no such asset".
const RELEASE_OBJECT_AND_ASSETS_JQ: &str = r#"(["R", .id, .tag_name,
      (.draft | tostring), .target_commitish] | @tsv),
    (.assets[]? | ["A", .name, (.id | tostring), (.size | tostring)] | @tsv)"#;

/// Split a [`RELEASE_OBJECT_AND_ASSETS_JQ`] response into the two row blocks its
/// tags name, stripping the tag so each block is exactly what its parser has
/// always been handed. An untagged or unknown row fails closed: the fused read
/// must never silently degrade into "this release has no assets".
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

/// The immutable release-object identity AND the exact-name asset binding, from
/// ONE read of `repos/{slug}/releases/{id}`.
///
/// Both facts live in the same JSON document, so the authoritative manifest scan
/// used to spawn two `gh` processes — two cold starts and two HTTPS round trips
/// — per end of every download bracket. Worse, the pair was SKEWED in time: a
/// mutation landing between the two reads was invisible to both checks. Fusing
/// them is therefore strictly tighter as well as strictly cheaper: each bracket
/// end is now a single point-in-time snapshot.
///
/// The checks a caller runs on the result, and their order, are deliberately
/// left to the caller so the existing scan's error precedence is unchanged.
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
    // A 404 is an ANSWER here, not a failure, so it must not burn the retry
    // budget (seven seconds of backoff to re-learn an absent release) — that is
    // `release_object_by_id`'s rule, and this call inherits it. Any OTHER
    // non-zero exit is the transient flake the asset-identity read absorbed
    // here has always retried through `gh_retry`, so it still retries.
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

/// The bracketed transfer itself. Exposed to the crate (not just to
/// [`download_release_asset_for_release_id`]) so the authoritative scan can hand
/// in a recheck that reads the asset binding and the release-object identity in
/// ONE call — see [`release_object_and_asset_identity`].
pub(crate) fn download_release_asset_with_identity_and_recheck(
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

/// Refuse a manifest that still names the RETIRED Intel DMG pair
/// (`dmg_x86_64` / `dmg_x86_64_sha256`, retired 2026-08-26 with the
/// batteries-included seed).
///
/// The two wire keys stay in the shared `Manifest` type so every client keeps
/// parsing the manifests already published under them; this cutter simply
/// never emits them, and every gate that judges a manifest — the draft
/// exact-set gate, the self-check, post-publish verify and a killed-machine
/// recovery — runs this first so a manifest from the retired contract is
/// refused by name rather than half-honoured by a set that no longer carries
/// the container it names.
pub fn refuse_retired_intel_dmg(manifest: &Manifest) -> Result<()> {
    if manifest.dmg_x86_64.is_some() || manifest.dmg_x86_64_sha256.is_some() {
        return Err(Error::new(format!(
            "manifest names the Intel DMG variant ({:?}) — retired 2026-08-26: aterm ships \
             ONE lean macOS DMG and this cutter neither produces nor mirrors an \
             `aterm-<v>-x86_64.dmg`. Finish or retire the cut that staged this manifest \
             with the cutter version that started it",
            manifest
                .dmg_x86_64
                .as_deref()
                .unwrap_or("<digest without a name>")
        )));
    }
    Ok(())
}

/// The exact asset set a draft may carry before it is allowed to become visible.
///
/// `roster_attached` is derived from the MANIFEST's own `machine_id`, not from a
/// local flag, at every call site: a release whose appcast claims a machine must
/// carry the roster that proves it, and a release whose appcast claims none must not
/// carry a roster at all. Deriving it from the published bytes is what keeps this a
/// total check — the draft is judged by what it says about itself.
pub fn validate_draft_asset_set(
    names: &[String],
    manifest: &Manifest,
    signature_required: bool,
    provenance_name: &str,
    dsym_name: Option<&str>,
) -> Result<()> {
    let roster_attached = manifest.machine_id.is_some();
    let count = |name: &str| {
        names
            .iter()
            .filter(|observed| observed.as_str() == name)
            .count()
    };
    // The manifest is the authority for the zip exactly as it is for the DMG: a
    // manifest that names a container the release does not carry would publish a
    // head every client resolves and then fails to download. Each container
    // carries its `.sha256` sidecar — the record the release notes tell a human
    // to verify the download against.
    let dmg_sidecar = mirror::sha256_sidecar_name(&manifest.dmg);
    let zip_sidecar = manifest.zip.as_deref().map(mirror::sha256_sidecar_name);
    // RETIRED 2026-08-26: the Intel `dmg_x86_64` pair. This cutter emits
    // neither wire key, so a manifest naming one is not this cutter's — refuse
    // before judging the draft against a container shape that no longer ships.
    refuse_retired_intel_dmg(manifest)?;
    let mut exact_counts = vec![
        (manifest_out::MANIFEST_ASSET, 1usize),
        (
            manifest_out::MANIFEST_SIG_ASSET,
            usize::from(signature_required),
        ),
        (manifest.dmg.as_str(), 1usize),
        (dmg_sidecar.as_str(), 1usize),
        (provenance_name, 1usize),
        (roster::ROSTER_ASSET, usize::from(roster_attached)),
        (roster::ROSTER_SIG_ASSET, usize::from(roster_attached)),
    ];
    if let Some(zip) = manifest.zip.as_deref() {
        exact_counts.push((zip, 1usize));
    }
    if let Some(sidecar) = zip_sidecar.as_deref() {
        exact_counts.push((sidecar, 1usize));
    }
    for (name, expected) in exact_counts {
        let observed = count(name);
        if observed != expected {
            return Err(Error::new(format!(
                "draft artifact set carries {observed} assets named {name:?}; expected {expected}"
            )));
        }
    }
    let mut dmgs: Vec<&str> = names
        .iter()
        .filter(|name| name.ends_with(".dmg"))
        .map(String::as_str)
        .collect();
    dmgs.sort_unstable();
    // ONE DMG: the manifest-named one, nothing else.
    let expected_dmgs: Vec<&str> = vec![manifest.dmg.as_str()];
    if dmgs != expected_dmgs {
        return Err(Error::new(format!(
            "draft artifact set has non-canonical DMG names {dmgs:?}; expected exactly \
             {expected_dmgs:?}"
        )));
    }
    let mut allowed = vec![
        manifest_out::MANIFEST_ASSET,
        manifest.dmg.as_str(),
        dmg_sidecar.as_str(),
        provenance_name,
    ];
    if let Some(zip) = manifest.zip.as_deref() {
        allowed.push(zip);
    }
    if let Some(sidecar) = zip_sidecar.as_deref() {
        allowed.push(sidecar);
    }
    if signature_required {
        allowed.push(manifest_out::MANIFEST_SIG_ASSET);
    }
    if roster_attached {
        allowed.push(roster::ROSTER_ASSET);
        allowed.push(roster::ROSTER_SIG_ASSET);
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

/// WHICH KEY must the shipped binary prove it compiled in?
///
/// `aterm-gui/build.rs` embeds `__DATA,__aterm_upin` from
/// `pins::update_channel_signing_pubkey()` — the committed keyset HEAD — because the
/// record exists to prove which ANCHOR reached the artifact, which is a property of
/// the source tree and not of the machine that ran the build. So the head is what
/// `buildplan` must expect.
///
/// It used to be derived from the SIGNING key instead, and that was correct only
/// while "the signer IS the head" was an invariant — which is exactly the invariant
/// the machine roster relaxes. Left alone it would have been a trap with a long fuse:
/// a rostered non-head machine would clear every pre-claim gate, burn a ledger
/// number, spend fifteen minutes building, and then fail the Mach-O pin proof with a
/// fingerprint mismatch that names neither the roster nor the keyset.
///
/// Nothing changes for either configuration that exists today, and that is checkable
/// rather than hopeful:
///
/// * PINNED CHANNEL — [`channel_signature_policy`] has already refused unless the
///   signing key is the head (unarmed), so on the shipped path `committed_head` and
///   `signing` are the same string and this returns the same fingerprint it always
///   did. ARMED, the two may legitimately differ — the roster authorizes machines the
///   keyset never carried — and taking the HEAD is what keeps this record a statement
///   about the source tree rather than about which laptop ran the build.
/// * UNPINNED CHANNEL (a fork) — there is no head, so the signing key remains the
///   expectation, byte for byte as before.
pub fn expected_embedded_update_pin(
    committed_head: Option<&str>,
    signing: Option<&str>,
) -> Result<Option<String>> {
    committed_head
        .or(signing)
        .map(update_key_fingerprint)
        .transpose()
}

/// Unix seconds for the roster's freshness window — fail-closed in the OPPOSITE
/// direction from [`unix_now`], and deliberately so.
///
/// `unix_now` returns 0 on an unreadable clock, which is right where it is used (a
/// zero timestamp reads as "long ago" and makes every deadline look passed). Here 0
/// would read as 1970, which is before every conceivable `valid_until`, so a LAPSED
/// roster would sail through the gate and the cut would publish a release the whole
/// fleet refuses. A clock we cannot read must therefore look like the far future,
/// which makes every window look expired and refuses the cut. This is the same
/// reasoning, and the same value, as `aterm_update::github::unix_now`.
fn roster_now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(i64::MAX, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The complete pre-claim signing verdict: WHETHER this cut signs, with WHICH key,
/// as WHICH machine, and under WHICH roster document.
///
/// One struct rather than a tuple because the three parts are only ever correct
/// together: an attribution without the roster bytes could not be published (the
/// client refuses a release whose roster assets are absent), and roster bytes
/// without an attribution would be assets nobody is authorized by.
///
/// `Debug` is derived rather than hand-written, and that is safe by construction:
/// every field is public information — a public key, a machine id, a published
/// roster and a detached signature. The redaction discipline lives where a secret
/// actually is, on `sign::ReleaseCredentials`, and this type deliberately does not
/// hold one.
#[derive(Debug)]
pub struct SigningVerdict {
    /// The verdict the pipeline has always carried.
    pub policy: SignaturePolicy,
    /// WHICH machine the roster says this key is, when the tier is armed. `None`
    /// with an unpinned master — which is every cut this tree can make.
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

    /// The three attribution-shaped fields a [`CutCtx`] carries, derived together.
    ///
    /// Together, because they are only correct together and the ways they can be wrong
    /// are silent: an id with no roster bytes stages no assets and publishes a release
    /// an armed client refuses structurally; roster bytes with no id attach a document
    /// nothing is authorized by. Handing the cut ONE value it destructures is what
    /// stops a future edit setting two of the three.
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
    /// Nothing to stamp and nothing to stage — every cut this tree makes, and every
    /// resume past `build`, where the manifest already carries its attribution and the
    /// roster assets are already on disk.
    const fn none() -> Self {
        Self {
            machine_id: None,
            attribution: None,
            roster: None,
        }
    }
}

/// May a resume that re-authorized as `observed` continue a cut the journal says was
/// started by `journaled`?
///
/// Only if they are the same machine — including "both nameless", which is every cut
/// made while the paper master is unpinned. The asymmetric cases are the interesting
/// ones and both must refuse:
///
/// * journaled `Some`, observed `None` — the cut was authorized by a roster and this
///   resume has none. Continuing would rebuild and re-sign a manifest whose
///   attribution nothing currently proves.
/// * journaled `None`, observed `Some` — the anchor was armed mid-cut. The already
///   published (or already built) bytes carry no attribution, so finishing under one
///   would produce a release whose halves disagree.
///
/// A pure rule with a name, rather than an inline `!=`, because it is the only part
/// of the resume path the empty anchor makes unreachable — and an unreachable rule
/// with no test is a rule that rots for exactly as long as nobody would notice.
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

/// The pipeline's entry point into the verdict: [`signing_verdict`] with THE anchors.
///
/// The anchors are named here and nowhere below, exactly as [`resolve_apple_tier`]
/// names `pins::APPLE_TEAM_ID` at the two cut entry points and passes it inward. That
/// is what makes the armed path drivable by a test with a synthetic master: a
/// resolver that read the constants itself would be inert in this tree — the master
/// is unpinned — and its rules would be untestable for exactly as long as they are
/// unused, which is exactly as long as nobody would notice them breaking.
fn preflight_signature_policy(
    repo: &Path,
    creds: Option<&sign::ReleaseCredentials>,
    duty: RosterDuty,
    pre_roster: PreRosterClients,
) -> Result<SigningVerdict> {
    signing_verdict(
        repo,
        creds,
        &SigningAnchors {
            master_pubkeys: aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
            committed_keyset: aterm_update_core::pins::UPDATE_CHANNEL_PUBKEYS,
            identity_path: machines::conventional_identity_path().as_deref(),
            now_unix: roster_now_unix(),
            duty,
            pre_roster,
        },
    )
}

/// Which [`RosterDuty`] a re-entry carries, from the one fact that decides it: has
/// `build` already run?
///
/// `build` is the only step that assembles a manifest, stamps an attribution into it
/// and signs it (`stage_manifest` → `sign_manifest_with_policy`), and the only step
/// that stages the roster assets. Every step after it moves bytes that already exist.
///
/// A named function rather than an inline `if` because three entry points must agree
/// on it — `resume_cut`, `run_recover_lost` and `revalidate_ctx_signature_policy` —
/// and the bug this closes was exactly those three disagreeing.
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
    /// `pins::UPDATE_CHANNEL_PUBKEYS` in production — the whole keyset.
    pub committed_keyset: &'a [&'a str],
    /// `~/.aterm/machine.toml` in production; `None` on a machine with no `HOME`.
    /// Consulted only when the profile declares no `machine_id`, and only ever as a
    /// cross-check.
    pub identity_path: Option<&'a Path>,
    /// Injected wall clock for the roster's freshness window.
    pub now_unix: i64,
    /// Whether this entry can still sign; see [`RosterDuty`].
    pub duty: RosterDuty,
    /// Whether this entry owes an answer for stranding clients that predate the
    /// roster, and if so what the operator said. See [`PreRosterClients`].
    pub pre_roster: PreRosterClients,
}

pub fn signing_verdict(
    repo: &Path,
    creds: Option<&sign::ReleaseCredentials>,
    anchors: &SigningAnchors<'_>,
) -> Result<SigningVerdict> {
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
    //
    // The machine-roster tier folds in HERE, at the same seam, for the same reason:
    // this function is the pipeline's one answer to "may this machine sign?", and a
    // second seam would be a second thing to keep in step. Its inputs are resolved
    // here and passed inward — `pins` stays the only place the anchors are named,
    // and `channel_signature_policy` stays a pure decision a test can drive with a
    // synthetic master.
    //
    // With `PAPER_MASTER_PUBKEYS` empty (this tree) the roster document is not even
    // READ: an unarmed anchor must cost nothing and must not fail a cut because a
    // profile mentions a file that has since moved.
    //
    // A `Finish` duty does not read it either, and for a related reason: the roster it
    // would read is not the roster the cut is publishing (that one is frozen in
    // `dist/`), so reading it could only produce a verdict about the wrong document.
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
        workspace_channel_pubkey(repo)?.as_deref(),
        load_signing_material(creds)?
            .as_ref()
            .map(|material| material.pubkey.as_str()),
        &RosterEvidence {
            master_pubkeys: anchors.master_pubkeys,
            committed_keyset: anchors.committed_keyset,
            roster: document.as_ref(),
            declared_machine_id: declared.as_deref(),
            now_unix: anchors.now_unix,
            duty: anchors.duty,
            pre_roster: anchors.pre_roster,
        },
    )?;
    Ok(SigningVerdict {
        // The roster document is kept only when it actually authorized something, so
        // "we have roster bytes" and "we have an attribution" can never disagree —
        // every later step keys off one of them and would otherwise have to trust
        // that the other agrees.
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
    // Signed IN-PROCESS. This used to spawn `atpkg-keys sign` with the private key's
    // PATH on the command line — visible in a process listing, and unusable unless a
    // second binary happened to be built.
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

/// Assemble the manifest, STAMP the attribution into it, prove the bytes, and write
/// them — in that order, which is the entire security property.
///
/// `machine_id` and `roster_seq` are worth nothing unless they are inside what the
/// signature covers. The signature is produced by `sign_manifest_with_policy`, which
/// reads the FILE this function wrote; so as long as the stamp happens before the
/// write, it is inside the signed bytes by construction. Stamp after the write and
/// the release ships an attribution any attacker can rewrite; stamp after the
/// SIGNATURE and the release ships a signature that does not verify at all, which
/// `sign_manifest_with_policy`'s own read-back check catches.
///
/// It exists as a named function rather than four lines inside `step_build` because
/// `step_build` needs a real universal build to run and this ordering therefore had
/// no covering test — the one property most in need of one.
pub fn stage_manifest(
    dist: &Path,
    inputs: &manifest_out::ManifestInputs<'_>,
    who: Option<&roster::Attribution>,
) -> Result<PathBuf> {
    let mut manifest = manifest_out::build(inputs);
    // With an unpinned paper master `who` is `None` on every cut, both keys stay
    // absent, and the emitted bytes are identical to what this cutter has always
    // produced. That is the fleet-safety requirement, not a nicety.
    if let Some(who) = who {
        machines::attribute(&mut manifest, who);
    }
    manifest_out::write(dist, &manifest)
}

/// Stage the master-signed roster beside the appcast, so a client can fetch the
/// document that authorizes the signature it is about to check.
///
/// These are the exact bytes the pre-claim gate verified, carried through the cut
/// rather than re-read: publishing a roster other than the one that authorized the
/// cut is the producer-side version of checking one document and using another.
///
/// `None` — the shipped state — writes nothing and removes nothing, so an unarmed
/// cut's `dist/` is exactly what it was. There is no "clean up a stale roster" branch
/// on purpose: `validate_draft_asset_set` refuses any asset outside the exact allowed
/// set, so a leftover file in `dist/` cannot become a published asset.
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
    // RETIRED 2026-08-26: the optional Intel DMG. A published manifest naming
    // one was not emitted by this cutter and is not a shape it can verify.
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
    /// The signing material, loaded ONCE from `--release-credentials` at the entry
    /// point and carried for the life of the cut. Never serialized: the Journal keeps
    /// only `signature_pubkey`, the public identity. A path would prove nothing —
    /// file contents change between reading and signing — so the identity is what is
    /// recorded and matched.
    pub credentials: Option<sign::ReleaseCredentials>,
    /// Tier APPLE, resolved ONCE at the entry point for exactly the reason
    /// `credentials` is: a cut must not be able to change what signed it halfway
    /// through. Resolution happens before the ledger claim, so a machine with no
    /// Developer-ID certificate or no notarytool credential fails while failing
    /// is still free — a claim burns a single-use build number, and discovering
    /// an empty keychain after that costs one.
    ///
    /// `AppleTier::Inactive` whenever `pins::APPLE_TEAM_ID` is empty, which is
    /// every build that ships today.
    pub apple: sign::AppleTier,
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
    /// The key the PUBLISHED artifacts must verify UNDER — see
    /// [`Journal::verify_pubkey`]. `None` means [`Self::signature_pubkey`], which
    /// is every cut except a cross-machine recovery of someone else's release.
    pub verify_pubkey: Option<String>,
    /// The machine the roster authorized, journaled beside the public key. Set on
    /// every real cut and restored from the journal on resume — including a resume
    /// past `build`, where [`CutCtx::attribution`] is deliberately not restored.
    /// That asymmetry is the point: the id is what every later step must keep
    /// AGREEING with, the full attribution is only needed to STAMP a manifest.
    pub signature_machine_id: Option<String>,
    /// The full attribution (id + key + `roster_seq`) for the one step that stamps
    /// it into the manifest, and the roster bytes that authorized it, to be
    /// published as assets. Both are `Some` only when a build will actually run
    /// under an armed anchor; a resume that will not rebuild neither stamps nor
    /// re-stages, and asking it for a roster it cannot use would convert a
    /// recoverable cut into an unrecoverable one — the same rule
    /// [`resume_apple_tier`] applies to the Developer-ID certificate.
    pub attribution: Option<roster::Attribution>,
    pub roster: Option<machines::RosterDocument>,
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
    /// `--no-paint-smoke`, carried to `step_selfcheck`. Never journaled: a
    /// resume re-earns the paint proof (and the CLI refuses the flag there,
    /// like every other cut flag).
    pub no_paint_smoke: bool,
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
    /// What PUBLISHED bytes must verify under. Falls back to this machine's own
    /// signing key, which is the same value on every cut that is not recovering
    /// another machine's release.
    fn verification_pubkey(&self) -> Option<&str> {
        self.verify_pubkey
            .as_deref()
            .or(self.signature_pubkey.as_deref())
    }

    /// THIS CUT'S bundle — under `dist/cut-app/`, never the dev install at
    /// `dist/aterm.app`. See [`bundle::staged_app_path`] for the release the live
    /// updater ate when these were the same path.
    fn app_path(&self) -> PathBuf {
        bundle::staged_app_path(&self.dist)
    }
    /// The DMG's `.sha256` sidecar in dist/ — written by `step_build` from the
    /// in-process digest, verified against the manifest by `step_selfcheck`.
    fn dmg_sha256_path(&self) -> PathBuf {
        self.dist
            .join(mirror::sha256_sidecar_name(&mirror::dmg_asset_name(
                &self.version,
            )))
    }
    fn zip_sha256_path(&self) -> PathBuf {
        self.dist
            .join(mirror::sha256_sidecar_name(&mirror::zip_asset_name(
                &self.version,
            )))
    }
    /// The stable download twins in dist/ — `aterm.dmg` / `aterm-mac.zip`,
    /// byte copies of the canonical containers. Staged by `step_build` from
    /// the FINAL packaged bytes; re-proved (and, on a pre-twin journal's
    /// resume, regenerated) by `step_selfcheck`; served to the public channel
    /// through `mirror_asset_paths`.
    fn stable_dmg_path(&self) -> PathBuf {
        self.dist.join(mirror::stable_dmg_asset_name())
    }
    fn stable_zip_path(&self) -> PathBuf {
        self.dist.join(mirror::stable_zip_asset_name())
    }
    /// The twins' `.sha256` sidecars — the SAME digests the versioned sidecars
    /// state, with the ALIAS filename embedded, because `shasum -a 256 -c`
    /// matches on the embedded name and a `releases/latest/download/...` click
    /// saves the alias name.
    fn stable_dmg_sha256_path(&self) -> PathBuf {
        self.dist
            .join(mirror::sha256_sidecar_name(&mirror::stable_dmg_asset_name()))
    }
    fn stable_zip_sha256_path(&self) -> PathBuf {
        self.dist
            .join(mirror::sha256_sidecar_name(&mirror::stable_zip_asset_name()))
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

    /// Undo an upload intent for a POST that PROVABLY never reached the network.
    ///
    /// The one-shot rule — record the intent, then never repeat a POST whose
    /// response was lost — is right, and it is why a resume refuses to re-upload an
    /// asset it cannot see. But it treats "the response was lost" and "the request
    /// was never sent" as the same state, and on 2026-08-19 the second one wedged a
    /// cut permanently: curl refused its own arguments (`--data-binary: out of
    /// memory` on a gigabyte DMG), so no socket was ever opened, yet every later
    /// resume declined to retry a POST that had never happened. The draft had zero
    /// assets; no supported command could finish the release; the number had to be
    /// abandoned.
    ///
    /// Only [`transport_never_started`] may lead here. That is a local, provable
    /// fact — curl's exit 2 is argument/initialisation failure, before connect — and
    /// nothing about it depends on what a server did or did not receive.
    fn retract_upload_intent(&mut self, name: &str) -> Result<()> {
        self.upload_intents.retain(|issued| issued != name);
        if let Some(journal) = &mut self.journal {
            journal.upload_intents.retain(|issued| issued != name);
            journal.save(&self.journal_path)?;
        }
        Ok(())
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

    /// The mirror twin of [`Self::retract_upload_intent`], under the same rule and
    /// for the same reason: only a POST that never reached the network.
    fn retract_mirror_upload_intent(&mut self, name: &str) -> Result<()> {
        self.mirror_upload_intents.retain(|issued| issued != name);
        if let Some(journal) = &mut self.journal {
            journal
                .mirror_upload_intents
                .retain(|issued| issued != name);
            journal.save(&self.journal_path)?;
        }
        Ok(())
    }

    /// Local paths of exactly the assets that cross to the public channel, in a
    /// stable order. Derived from the same [`mirror::required_asset_names`] the
    /// remote listing is checked against, so the upload set and the acceptance
    /// rule cannot drift apart.
    fn mirror_asset_paths(&self) -> Vec<PathBuf> {
        mirror::required_asset_names(
            &self.version,
            self.signature_required,
            self.attaches_roster(),
        )
        .into_iter()
        .map(|name| self.dist.join(name))
        .collect()
    }

    /// Does this cut publish the machine roster?
    ///
    /// Exactly when it has an attributed machine, which is exactly when the paper
    /// master is pinned — and the answer is read from the JOURNALED id rather than
    /// from the in-memory roster document so that it survives a resume past `build`,
    /// which has the assets on disk and no document in hand.
    fn attaches_roster(&self) -> bool {
        self.signature_machine_id.is_some()
    }

    /// The two roster assets in `dist/`, or nothing at all on the shipped path.
    fn roster_asset_paths(&self) -> Vec<PathBuf> {
        if !self.attaches_roster() {
            return Vec::new();
        }
        vec![
            self.dist.join(roster::ROSTER_ASSET),
            self.dist.join(roster::ROSTER_SIG_ASSET),
        ]
    }

    /// Every local artifact that must reach the DRAFT release, in upload order.
    ///
    /// A named set rather than a `vec![]` inside `step_upload` because the roster's
    /// membership in it is a fleet-safety property with no other test: an armed client
    /// refuses a release carrying no `aterm-machines.toml` structurally, before any
    /// artifact crypto, and the updater has no fallback to an older release — so a
    /// regression that silently stopped attaching the pair would publish a well-formed
    /// release that wedges the fleet, and nothing would fail.
    fn upload_asset_paths(&self) -> Vec<PathBuf> {
        let mut files = vec![
            self.dmg_path(),
            self.dmg_sha256_path(),
            self.zip_path(),
            self.zip_sha256_path(),
            self.manifest_path(),
            self.provenance_path(),
        ];
        files.extend(self.roster_asset_paths());
        files
    }

    /// Every local artifact whose bytes the draft proof compares against the remote
    /// object, minus the dSYM (which is present only when the build produced one).
    ///
    /// The roster has to be in here and not merely in the asset-NAME check: the
    /// appcast's signature does not cover it — the master's does — so nothing else in
    /// that proof would notice a roster replaced remotely between upload and flip.
    fn proof_asset_paths(&self) -> Vec<PathBuf> {
        let mut files = vec![
            self.manifest_path(),
            self.dmg_path(),
            self.dmg_sha256_path(),
            self.zip_path(),
            self.zip_sha256_path(),
            self.provenance_path(),
        ];
        if self.signature_required {
            files.push(self.manifest_path().with_extension("toml.sig"));
        }
        files.extend(self.roster_asset_paths());
        files
    }

    /// WHICH fingerprint the shipped binary must prove it compiled in.
    ///
    /// One accessor so `step_build` (which sets the expectation and writes it into the
    /// provenance) and `step_selfcheck` (which checks the binary and the provenance
    /// against it) cannot derive it differently. They did: the build followed the
    /// committed head and the self-check followed the signing key, which agree only
    /// while signer == head — the invariant the machine roster relaxes.
    fn expected_embedded_pin(&self) -> Result<Option<String>> {
        expected_embedded_update_pin(
            workspace_channel_pubkey(&self.repo)?.as_deref(),
            self.signature_pubkey.as_deref(),
        )
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
    // The remote's own binding, for the lost-journal recovery path: a draft under this
    // tag that targets the claim commit is provably this claim's object.
    let claim_bound = by_tag
        .as_ref()
        .is_some_and(|release| release.target_commitish == lease.owner());
    match draft_cleanup_decision(create_intent_knowledge, by_tag.is_some(), claim_bound) {
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
    operator_asserts_no_post: bool,
    credentials: Option<&sign::ReleaseCredentials>,
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
    // A recovery's duty is read off the journal it found, not assumed: a journal that
    // never reached `build` will rebuild and re-sign, so it must re-prove the roster;
    // one that is past `build` — and the no-journal case, which is a PUBLISHED release
    // being finished — has nothing left to sign. Demanding a still-fresh roster from
    // the second kind would make recovery fail for a reason unrelated to recovering,
    // which is the trade the `AppleTier::Inactive` decision below already refuses to
    // make for an expired certificate.
    let duty = roster_duty(journal.as_ref().is_none_or(|j| j.is_done("build")));
    // A RECOVERY never begins a cut; it continues one whose signing key it is not
    // permitted to change. The pre-roster question was answered at that cut's pre-claim.
    let signature_verdict =
        preflight_signature_policy(repo, credentials, duty, PreRosterClients::Answered)?;
    let signature_policy = signature_verdict.policy.clone();
    if let Some(journal) = &journal
        && (journal.signature_required != signature_policy.required
            || journal.signature_pubkey.as_deref() != signature_policy.pubkey.as_deref())
    {
        return Err(Error::new(
            "recovery journal signing policy/key differs from the current signing configuration",
        ));
    }
    // Attribution is compared on exactly the same terms as the key — and ONLY when this
    // recovery will rebuild. A recovery that will re-assemble and re-sign a manifest is
    // choosing an attribution, and choosing a different one than the journal records
    // would publish a claim the cut's own history contradicts; that is what this
    // refuses. A recovery that is finishing already-signed bytes chooses nothing, so
    // there is nothing to disagree about, and refusing there would kill the one path
    // designed for a DEAD PUBLISHER in the one design where publishers are plural. The
    // rule is the shared pure one so that recovery and resume cannot state it
    // differently; the old inline `is_some()` guard was one-sided and did.
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
                ResumePaths {
                    repo,
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
                create_intent_knowledge,
                expected_release_id,
                abandoned_journal,
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
    create_intent_knowledge: Option<bool>,
    expected_release_id: Option<u64>,
    abandoned_journal: Option<&'a Path>,
    /// [`RECOVERY_NO_DRAFT_POSTED_FLAG`] was given.
    operator_asserts_no_post: bool,
}

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
        create_intent_knowledge,
        expected_release_id,
        abandoned_journal,
        operator_asserts_no_post,
    } = plan;
    let git = GitCli::new(repo);
    let lease = confirm_release_lease_owner(&git, owner)?;
    assert_publisher_session(&git, &lease, fence)?;
    let tag = format!("v{version}");
    match verify::release_state(slug, &tag)? {
        verify::ReleaseState::Published => {
            let fresh_policy =
                fresh_published_recovery_signature_policy(repo, slug, version, credentials)?;
            recover_published_cut(
                repo,
                slug,
                version,
                build,
                owner,
                &fresh_policy,
                lease,
                fence.clone(),
                credentials,
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
                    if absent_draft_decision(create_intent_knowledge, operator_asserts_no_post)
                        == AbsentDraftDecision::RetainOwnerAwaitVisibility
                    {
                        let (why, remedy) = if create_intent_knowledge == Some(true) {
                            (
                                "known issued",
                                "wait for the exact draft to converge and run recover again",
                            )
                        } else {
                            (
                                "unknown because the current journal is unavailable",
                                "if the releases page shows NO draft for this tag, re-run with \
                                 --no-draft-was-posted to release the claim lease",
                            )
                        };
                        return Err(Error::new(format!(
                            "release {tag} is currently absent, but draft-create intent is \
                             {why}. An accepted POST may still become visible; retaining the \
                             claim lease and refusing tag/journal cleanup until the exact draft \
                             converges — {remedy}"
                        )));
                    }
                }
                verify::ReleaseState::Published => {
                    let fresh_policy = fresh_published_recovery_signature_policy(
                        repo,
                        slug,
                        version,
                        credentials,
                    )?;
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
    credentials: Option<&sign::ReleaseCredentials>,
) -> Result<SignaturePolicy> {
    let state = verify::release_state(slug, &format!("v{version}"))?;
    if state != verify::ReleaseState::Published {
        return Err(Error::new(format!(
            "recovery release v{version} changed away from Published while refreshing its signature authority"
        )));
    }
    // Only the POLICY half is refreshed here. A published recovery rebuilds nothing —
    // it validates and finishes bytes that already shipped — so the attribution it
    // must record is the one INSIDE those bytes, read from the downloaded manifest by
    // `recover_published_cut`, never a fresh local claim about who this machine is.
    Ok(preflight_signature_policy(
        repo,
        credentials,
        RosterDuty::Finish,
        PreRosterClients::Answered,
    )?
    .policy)
}

/// Which assets a published release must be carrying for its own manifest to make
/// sense, beyond the ones every release has.
///
/// Derived from the MANIFEST's `machine_id` — what the release says about itself —
/// rather than from local state, exactly as `validate_draft_asset_set` derives the same
/// requirement. A recovery that judged the release by the recovering machine's
/// configuration would reconstruct one set and mirror another.
fn recovered_roster_asset_names(manifest: &Manifest) -> Vec<&'static str> {
    if manifest.machine_id.is_none() {
        return Vec::new();
    }
    vec![roster::ROSTER_ASSET, roster::ROSTER_SIG_ASSET]
}

/// Rebuild `dist/`'s roster pair from the published release, for a recovery that found
/// an attributed manifest.
///
/// # Why this is not optional
///
/// `recover_published_cut` sets `signature_machine_id` from the recovered manifest —
/// correctly, out of bytes a signature covers — and that makes `CutCtx::attaches_roster`
/// true, which puts both roster names into `mirror_asset_paths`. `step_mirror` hard-errors
/// on any mirror asset that is not a local file. Reconstructing the DMG, the zip, the
/// appcast, its signature and the provenance but not these two therefore left
/// `cargo ship recover-lost` unable to complete on the armed path: it failed with
/// "…dist/ artifacts are gone… recover the cut rather than mirroring different bytes",
/// whose advice is the command that was already running. The release stayed live on the
/// publish repo and absent from the public channel the fleet actually reads.
///
/// # What binds them, given the manifest's signature does not
///
/// There is no SHA-256 for these two in the manifest — the MASTER signs them, not the
/// release key — so they are bound cryptographically instead, by
/// `machines::verify_published_roster`: the master signature proves authorship, and the
/// `roster_seq`/`machine_id` pair proves it is THIS release's roster. That is strictly
/// stronger than the digest check the other assets get.
/// Refuse to reconstruct a roster OLDER than the one this machine is already
/// authorized by.
///
/// `dist/aterm-machines.toml` is not a build artifact. It is the machine's
/// AUTHORIZING roster — the same file `atpkg-keys` writes and
/// `ReleaseCredentials::resolve` adopts — and `dist/` is gitignored, so it is the
/// only copy on the machine. Recovery used to overwrite it unconditionally with
/// whatever generation the recovered release happened to carry, which silently
/// DOWNGRADES it: revoke a stolen machine (seq N+1, written locally, not yet
/// published), then recover an older cut made under seq N, and the revocation is
/// gone — recreatable only by re-entering the 52-character paper master.
///
/// Nothing reported it, either. `roster_floor_covered` compares the carried
/// generation against the published head, and after such a recovery both are N, so
/// the next cut from this machine re-publishes a roster that still authorizes the
/// machine the owner had just revoked.
///
/// A recovery may reconstruct the release's roster. It may not retire a newer one.
/// The public key a PUBLISHED release's manifest signature must actually verify
/// under.
///
/// Recovery validates bytes that ALREADY SHIPPED, so the verification key is a
/// property of the release, not of the machine running the command. A
/// [`RosterDuty::Finish`] policy carries this machine's own key — right for an entry
/// that will still sign something, wrong here — and using it made cross-machine
/// recovery structurally impossible: m3 cuts v0.24.0 and dies after the flip, the
/// owner runs `recover` on m11, and m3's shipped signature is checked against m11's
/// key. It fails with "manifest signature does not verify under the channel public
/// key", the release never reaches the public channel, and
/// `refs/tags/aterm-release-lease` stays held by the dead machine — so every later
/// `cargo ship cut` refuses at `preflight_release_lease` with no command able to
/// un-wedge it.
///
/// On the armed path the release names its own signer and the master-signed roster
/// beside it maps that name to a key: the same binding a client checks
/// (`Attribution::bind`). Revocation is deliberately NOT re-judged, for the reason
/// [`machines::verify_published_roster`] gives about this exact document — these
/// bytes are already published, and revoking a machine afterwards does not
/// retroactively unsign what it signed.
///
/// A release with no `machine_id` predates the roster tier; there the committed
/// channel keyset is the authority, which is what the policy already carries.
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
    // Proves authorship (paper master) AND that this is THIS release's roster — the
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

fn refuse_roster_downgrade(dist: &Path, incoming_seq: u64) -> Result<()> {
    let local = dist.join(roster::ROSTER_ASSET);
    let (Ok(bytes), Ok(sig)) = (
        fs::read(&local),
        fs::read(dist.join(roster::ROSTER_SIG_ASSET)),
    ) else {
        // No local pair (or half of one): there is nothing here to protect.
        return Ok(());
    };
    // Only a MASTER-SIGNED local roster can outrank the release's. An unverifiable
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
            "{} already holds roster_seq {}, which is NEWER than the roster_seq \
             {incoming_seq} carried by the release being recovered. Overwriting it \
             would destroy the only copy of a master-signed generation — including \
             any revocation it carries — and it can be recreated only from the paper \
             master. Publish the newer roster first (so the channel head carries it), \
             or move the pair aside deliberately if you really do mean to go back",
            local.display(),
            existing.roster_seq
        )));
    }
    Ok(())
}

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
    // The ASSET's generation is what gets written — it may be newer than the
    // manifest's attribution after a join re-dressed the release, and the local
    // no-downgrade check must compare against the bytes actually landing in dist/,
    // not the number the manifest names (comparing the manifest seq refused a
    // recovery from any machine already holding the joined generation).
    let incoming_seq = machines::verify_published_roster(
        aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        roster_bytes.clone(),
        &roster_sig,
        machine_id,
        manifest.roster_seq,
    )?;
    refuse_roster_downgrade(dist, incoming_seq)?;
    // Through the roster PAIR's writer lock and redo transaction, not two bare writes:
    // this is the same `dist/aterm-machines.toml` + `.sig` that `cargo ship provision`
    // seeds and the mint re-signs, and a death between two `fs::write`s left exactly the
    // torn pair — new document, old signature — that no client verifies and every
    // operator reads as a bad phrase. `crate::provision::publish_proven_pair` takes the
    // lock for this one write; the bytes were proved under the pinned paper master by
    // `verify_published_roster` above.
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
    validate_release_object_capability(
        Some(&release_object),
        release_object.id,
        &tag,
        owner,
        false,
    )?;
    let (manifest_bytes, signature_bytes) =
        download_live_manifest_pair(slug, release_object.id, &tag)?;
    // The key comes from the RELEASE, never from this machine — see
    // `published_manifest_signature_pubkey` for the cross-machine recovery this
    // un-wedges.
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
    // Regenerate the stable download twins from the digest-verified canonical
    // containers: recovery must leave dist/ able to satisfy
    // required_asset_names(), and each twin is by definition a byte copy of
    // its canonical asset: `aterm.dmg` is a byte copy of manifest.dmg, the ONE
    // DMG (RETIRED 2026-08-26: the lean/seeded split the alias once tracked).
    fs::copy(
        dist.join(&manifest.dmg),
        dist.join(mirror::stable_dmg_asset_name()),
    )
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
        dist.join(mirror::stable_zip_asset_name()),
    )
    .map_err(|error| Error::new(format!("reconstruct stable zip twin: {error}")))?;
    // RETIRED 2026-08-26: the Intel DMG pair. A published release whose
    // manifest still names one was cut by a previous cutter under a container
    // contract this one no longer produces or mirrors — refuse the takeover
    // rather than reconstruct a dist/ the mirror's exact-set gate would then
    // refuse anyway.
    refuse_retired_intel_dmg(&manifest)?;
    // The `.sha256` sidecars are pure functions of the manifest digests just
    // proved against the downloaded bytes, so a recovery REGENERATES them
    // rather than downloading — the mirror step demands them from dist/ and a
    // release published before sidecars existed can still be recovered. The
    // twins' ALIAS sidecars are the same proved digests under the alias names,
    // so the documented `shasum -a 256 -c` works on the files the evergreen
    // `releases/latest/download` URLs actually save.
    let stable_dmg_name = mirror::stable_dmg_asset_name();
    let stable_zip_name = mirror::stable_zip_asset_name();
    let sidecar_records = [
        (manifest.dmg.as_str(), manifest.sha256.as_str()),
        (recovered_zip.as_str(), recovered_zip_sha256.as_str()),
        (stable_dmg_name.as_str(), manifest.sha256.as_str()),
        (stable_zip_name.as_str(), recovered_zip_sha256.as_str()),
    ];
    for (name, sha) in sidecar_records {
        let sidecar = mirror::sha256_sidecar_name(name);
        fs::write(
            dist.join(&sidecar),
            mirror::sha256_sidecar_contents(sha, name),
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
    fs::write(dist.join(&provenance_name), provenance)
        .map_err(|error| Error::new(format!("reconstruct provenance: {error}")))?;
    reconstruct_roster_assets(slug, release_object.id, &names, &manifest, &dist)?;
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
        // FROM THE RELEASE, not from this machine: the artifacts being recovered are
        // already signed, by a machine that may not be this one. `signature_pubkey`
        // above stays this machine's own key so the local guards keep comparing like
        // with like (2026-08-19 round-6 audit).
        verify_pubkey: recovered_pubkey.clone(),
        // FROM THE PUBLISHED BYTES, not from this machine. The manifest being
        // recovered is already signed, and `machine_id` is inside what that
        // signature covers — so the only truthful answer to "which machine cut
        // this?" is the one the artifact itself carries. Deriving it locally would
        // let a recovery relabel someone else's release.
        signature_machine_id: manifest.machine_id.clone(),
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
        credentials: credentials.cloned(),
        // A recovered cut has already published; every build step is marked done
        // and none will run again, so there is nothing left for the tier to
        // sign. Resolving it here would make recovery — the path taken when
        // something has ALREADY gone wrong — fail for a reason unrelated to
        // recovering, e.g. a certificate that expired since the cut shipped.
        apple: sign::AppleTier::Inactive,
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
        verify_pubkey: recovered_pubkey.clone(),
        signature_machine_id: manifest.machine_id.clone(),
        // Nothing left to stamp and nothing left to stage: every build step is
        // already marked done, so the manifest that would carry an attribution and
        // the assets that would carry a roster are both already published bytes.
        attribution: None,
        roster: None,
        release_id: Some(release_object.id),
        draft_create_issued: true,
        upload_intents: Vec::new(),
        mirror_slug: workspace_mirror_slug(repo)?,
        mirror_release_id: None,
        mirror_create_issued: false,
        mirror_upload_intents: Vec::new(),
        kind: CutKind::Real,
        // Recovery re-runs no build/selfcheck step (all are journaled done),
        // so there is no smoke left to skip.
        no_paint_smoke: false,
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
    // Resolved ONCE, here — the explicit flag when given, else this machine's
    // provisioned identity (`~/.aterm/machine.key`, the same file every atpkg
    // producer tool signs with). Every later stage — build, resume, recovery,
    // flip — reads this value rather than re-discovering credentials, so a cut
    // cannot change identity halfway through.
    let credentials = sign::ReleaseCredentials::resolve(opts.release_credentials.as_deref(), repo)
        .map_err(Error::new)?;
    let credentials = credentials.as_ref();
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
        return resume_cut(
            ResumePaths {
                repo,
                dist: &dist,
                journal_path: &journal_path,
            },
            &origin_slug,
            j,
            t0,
            None,
            credentials,
        );
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

    // Tier APPLE, resolved HERE: after the resume delegation above (a resume
    // resolves its own tier, under its own rule — see `resume_apple_tier`) and
    // well before the gates, the lease and the claim. If the anchor is set, this
    // is where "is there a Developer-ID certificate for the committed team, and a
    // notarytool credential to submit with?" gets answered. Everything that can
    // fail must fail before the ledger claim burns a build number
    // (docs/RELEASE-KEYS.md's ordering rule); this is a fresh cut, so `build`
    // will certainly run and the tier is certainly needed.
    let apple = resolve_apple_tier(aterm_update_core::pins::APPLE_TEAM_ID, credentials)?;

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

    // RETIRED 2026-08-26: the pre-claim toolchain-seed gate (`dist/toolchain-seed`
    // validation, `ATERM_SEEDLESS=1`). aterm ships ONE lean self-provisioning
    // download; there is nothing to seal and nothing to gate.
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
    // THE MACHINE-ROSTER GATE RUNS HERE, inside this call, and here is deliberate:
    // it is the last of the pre-claim gates and it sits BEFORE the ledger claim a few
    // lines below. A claim burns a single-use build number and is pushed to origin, so
    // a refusal after it costs a number that can never be reused and leaves a dangling
    // claim for `cargo ship status` to explain. Everything the roster gate needs is
    // local — a file, a signature, a clock — so there is no reason for it to happen
    // one line later than the cheapest gates, and every reason for it not to.
    // THE ONE ENTRY THAT BEGINS A CUT, and therefore the only one that owes an answer
    // for the pre-roster fleet. Every re-entry below inherits it by inheriting the key.
    let signature_verdict = preflight_signature_policy(
        repo,
        credentials,
        RosterDuty::Sign,
        if opts.strand_pre_roster_clients {
            PreRosterClients::Stranded
        } else {
            PreRosterClients::Protected
        },
    )?;
    let signature_policy = signature_verdict.policy.clone();
    step(
        "signature",
        &match (workspace_channel_pubkey(repo)?, signature_policy.required) {
            (Some(pin), _) => format!(
                "committed channel anchor (aterm-update-core::pins) pins signing to \
                 {pin} · configured key matches"
            ),
            (None, true) => {
                "signing key configured · matches persisted public identity".to_string()
            }
            (None, false) => "no committed channel anchor and no signing configuration".to_string(),
        },
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
    // ask; a rehearsal/dry run keeps the manifest-only floor.
    let manifest_roster_seq = published_roster_seq(newest_channel)?;
    let observed_roster = match (&mirror_slug, kind) {
        (Some(slug), CutKind::Real) if *slug != origin_slug => {
            machines::channel_roster_document(slug).map_err(|e| {
                Error::new(format!(
                    "cannot read the machine roster on the public channel {slug}'s latest \
                     release ({e}); refusing to reason about the fleet's roster floor — a \
                     wrong answer here burns a build number and strands every client"
                ))
            })?
        }
        _ => None,
    };
    let observed_roster_seq = observed_roster.as_ref().map(|(seq, _)| *seq);
    // EQUAL generation, DIFFERENT document = a lineage fork; the number admits it, only
    // the bytes can refuse it. Compared before the claim for the same reason as the
    // ratchet: after it, the number is burned.
    // The bytes that AUTHORIZED this cut and will ship as its assets — not whatever
    // dist/ happens to hold (a stale leftover pair would be a false fork; an absent
    // one would skip the check until after the claim).
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
    // Announced only when the tier is ARMED. An inert anchor must add zero transcript
    // lines, exactly as Tier APPLE does — a cut from this tree has to look precisely
    // as it did before the roster existed.
    if let Some(who) = &signature_verdict.attribution {
        step(
            "roster",
            &format!(
                "machine {} authorized by the paper-master roster (roster_seq {} · \
                 channel head {}) (pre-claim)",
                who.machine_id,
                who.roster_seq,
                newest_roster_seq.map_or_else(|| "none".to_string(), |seq| seq.to_string())
            ),
        );
    }
    let attributed = signature_verdict.cut_attribution();

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
        credentials: credentials.cloned(),
        apple,
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
        // This machine signs and this machine verifies: one key, no split.
        verify_pubkey: None,
        signature_machine_id: attributed.machine_id,
        attribution: attributed.attribution,
        roster: attributed.roster,
        release_id: None,
        draft_create_issued: false,
        upload_intents: Vec::new(),
        mirror_slug: mirror_slug.clone(),
        mirror_release_id: None,
        mirror_create_issued: false,
        mirror_upload_intents: Vec::new(),
        kind,
        no_paint_smoke: opts.no_paint_smoke,
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
            verify_pubkey: ctx.verify_pubkey.clone(),
            signature_machine_id: ctx.signature_machine_id.clone(),
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
/// The three PATHS a resume works over, bundled because they always travel
/// together and are always derived from the same repo root — passing them
/// singly is what pushed this signature past the argument-count bar.
struct ResumePaths<'a> {
    repo: &'a Path,
    dist: &'a Path,
    journal_path: &'a Path,
}

fn resume_cut(
    paths: ResumePaths<'_>,
    origin_slug: &str,
    journal: Journal,
    t0: Instant,
    recovered_session: Option<(ReleaseLeaseGuard, PublisherFenceGuard)>,
    credentials: Option<&sign::ReleaseCredentials>,
) -> Result<()> {
    let ResumePaths {
        repo,
        dist,
        journal_path,
    } = paths;
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
        let material = load_signing_material(credentials)?.ok_or_else(|| {
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

    // THE ROSTER, re-derived under exactly the rule above it: only a resume that
    // will REBUILD needs it, because only `step_build` stamps a manifest and stages
    // the roster assets. A resume past `build` is finishing bytes that already carry
    // both, and demanding a still-fresh roster from it would turn a cut that is one
    // upload from done into one that can never be finished — the same trade
    // `resume_apple_tier` makes for an expired certificate, for the same reason.
    //
    // Re-deriving rather than trusting the journal is the point: the roster may have
    // lapsed or revoked this machine since the cut began, and a journal cannot know
    // that. What the journal DOES know is which machine started the cut, and a
    // resume that authorizes as a different machine is refused outright — the
    // manifest's attribution is inside bytes that are already signed, so a second
    // machine finishing the first machine's cut would publish a claim the artifact
    // contradicts.
    //
    // The `roster_tier_armed()` guard is what makes the unarmed resume path provably
    // unchanged: with `PAPER_MASTER_PUBKEYS` empty this block is not entered at all,
    // so a resume performs exactly the calls, in exactly the order, that it always
    // has. The RULE inside it is a pure function ([`resume_attribution_agrees`]) so
    // that being unreachable in this tree does not make it untested.
    let resumed = if aterm_update_core::pins::roster_tier_armed() && !journal.is_done("build") {
        let verdict = preflight_signature_policy(
            repo,
            credentials,
            RosterDuty::Sign,
            PreRosterClients::Answered,
        )?;
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

    // Symmetric with the signing-key re-proof directly above, under the same
    // `!is_done("build")` rule: a resume that can still rebuild artifacts must
    // re-prove it can still sign and notarize them, and a resume that cannot must
    // not be asked for credentials it will never use. Re-resolving rather than
    // trusting the journal is deliberate — the certificate could have expired or
    // been removed since the cut began, and a journal cannot know that.
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
        // A resume of a RECOVERED cut must keep verifying under the release's key,
        // not this machine's; `None` on every ordinary journal.
        verify_pubkey: journal.verify_pubkey.clone(),
        // The ID comes from the JOURNAL on every resume, including one past `build`:
        // it is the cut's fixed identity and every later step must keep agreeing with
        // it. The full attribution and the roster bytes come from the re-derivation
        // and are therefore `None` on a resume that will not rebuild — nothing left
        // to stamp, nothing left to stage.
        signature_machine_id: journal.signature_machine_id.clone(),
        attribution: resumed_attribution.attribution,
        roster: resumed_attribution.roster,
        release_id: journal.release_id,
        draft_create_issued: journal.draft_create_issued,
        upload_intents: journal.upload_intents.clone(),
        mirror_slug: workspace_mirror_slug(repo)?,
        mirror_release_id: journal.mirror_release_id,
        mirror_create_issued: journal.mirror_create_issued,
        mirror_upload_intents: journal.mirror_upload_intents.clone(),
        kind: CutKind::Real,
        // Never journaled: a resumed self-check re-earns the paint proof.
        no_paint_smoke: false,
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
    // journaled. The exceptions: an unlock-only resume (absence may mean the
    // delete landed and the journal mark crashed, so reacquiring would undo
    // convergence) and a post-unlock resume (`site` — the lease was already
    // CAS-deleted by this cut's own `unlock`; reacquiring would mint a lock
    // that nothing in the remaining steps ever deletes).
    if ctx.kind == CutKind::Real
        && !matches!(
            ctx.journal.as_ref().and_then(Journal::first_incomplete),
            Some(step) if step == "unlock" || is_post_unlock_step(step)
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
        if ctx.kind == CutKind::Real
            && !matches!(name, "lock" | "unlock")
            && !is_post_unlock_step(name)
        {
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
            "site" => {
                // The rehearsal publishes to a scratch repo the public site
                // must never link; only a real cut moves alab.systems.
                if ctx.kind == CutKind::Real {
                    step_site(ctx)?;
                }
            }
            _ => unreachable!("unknown pipeline step {name}"),
        }
        ctx.mark(name)?;
    }

    match ctx.kind {
        CutKind::Real => {
            // THE DEV INSTALL TAKES ITS OWN RELEASE — last, and only now. This
            // machine runs `dist/aterm.app` and its updater watches it, so placing
            // the bundle any earlier hands a live process something unfinished; that
            // is exactly how a cut got its sealed toolchain deleted mid-package
            // (see `bundle::staged_app_path`). A failure here costs nothing that
            // matters: the release is already live, verified and mirrored, and the
            // only casualty is this machine's convenience, so it warns rather than
            // failing a completed cut.
            match bundle::place_finished_bundle(&ctx.dist, &ctx.version, ctx.build) {
                Ok(bytes) => step(
                    "place",
                    &format!(
                        "dist/aterm.app \u{2190} this cut's verified bundle ({}) \u{2014} the dev \
                         install updates into it",
                        atpkg::human_bytes(bytes)
                    ),
                ),
                Err(error) => step(
                    "place",
                    &format!(
                        "WARNING: the release is live, but dist/aterm.app was not updated to it \
                         ({error}); this machine keeps running the older bundle"
                    ),
                ),
            }
            step(
                "DONE",
                &format!(
                    "v{} (build {}) — fleet stages within 6h.  [{}]  state: dist/cut-state.toml",
                    ctx.version,
                    ctx.build,
                    fmt_elapsed(t0)
                ),
            );
        }
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
    // The DUTY, from the same fact `resume_cut` and `run_recover_lost` read it from.
    // This used to be unconditional, and `resume_cut`'s own comment promised otherwise:
    // it guards its roster re-derivation with `!is_done("build")` and says demanding a
    // still-fresh roster from a later resume "would turn a cut that is one upload from
    // done into one that can never be finished". That promise was defeated here, four
    // hundred lines away, because `run_pipeline` calls this on every real entry whose
    // first incomplete step is not `unlock`. See [`RosterDuty`] for why the post-build
    // check is not merely inconvenient but wrong: the roster it would read is not the
    // roster the cut will publish.
    let duty = roster_duty(ctx.is_done("build"));
    let observed = preflight_signature_policy(
        &ctx.repo,
        ctx.credentials.as_ref(),
        duty,
        PreRosterClients::Answered,
    )?;
    if observed.policy.required != ctx.signature_required
        || observed.policy.pubkey.as_deref() != ctx.signature_pubkey.as_deref()
    {
        return Err(Error::new(
            "local signing configuration or the committed channel pin changed after this \
             cut's pre-claim scan; refusing to build/upload/flip under stale signing state",
        ));
    }
    // On a SIGN entry the roster has just been re-proved by the call above — freshness
    // and revocation included — and this adds the identity half: the machine that will
    // stamp and sign must be the machine the journal already records. A roster that
    // lapses or revokes this machine before `build` is a genuine refusal; the release
    // it would produce is one every armed client rejects, so failing here is strictly
    // better than failing in the fleet.
    //
    // On a FINISH entry `observed.machine_id()` is `None` by construction and nothing
    // is compared: the attribution is already inside signed bytes and no local verdict
    // can change it. This is what lets ANY rostered machine finish a dead publisher's
    // released cut — the case a plural-publisher design exists for.
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

/// What the website hook's exit status means for the `site` step. The codes
/// are `publish/post-promote`'s documented contract (its header comment):
/// 0 synced-or-deferred, 3 no site checkout, 4 deployed but the live site
/// lags the CDN — and of those only a code OUTSIDE the contract (1 hard
/// failure, 2 usage, a signal) fails the step. Pure so the contract is pinned
/// by tests without running the hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteHookOutcome {
    /// Exit 0 — synced, already current, or the hook's own narrated deferral
    /// (e.g. "SITE NOT DEPLOYED — committed and pushed").
    Synced,
    /// Exit 3 — no usable site checkout on this machine; nothing was touched
    /// and no retry HERE can ever succeed. Deferred loudly, step completes.
    NoSiteCheckout,
    /// Exit 4 — deployed, but the live origin still served old bytes after
    /// the settle loop. The deploy succeeded; re-check by hand.
    LiveLagging,
    /// Anything else — a real failure; the step fails and the journal parks
    /// at `site` for `cut --resume`.
    Failed,
}

#[must_use]
pub const fn site_hook_outcome(code: Option<i32>) -> SiteHookOutcome {
    match code {
        Some(0) => SiteHookOutcome::Synced,
        Some(3) => SiteHookOutcome::NoSiteCheckout,
        Some(4) => SiteHookOutcome::LiveLagging,
        _ => SiteHookOutcome::Failed,
    }
}

/// Journal step "site": alab.systems follows the cut — the download button
/// names the `aterm-<version>.dmg` the mirror just flipped live, with its true
/// size, and `/releases` carries the notes. The mechanism is the SAME hook a
/// `pub promote` runs (`publish/post-promote`, byte transforms in
/// `publish/site-sync.py`, tested hermetically by `tools/test-site-sync.sh`);
/// running it again here is what closes the promote-time gap that hook prints
/// as "v<version> not cut yet".
///
/// Runs after `unlock`, on a real cut only (see [`STEPS`]): the release is
/// already live, verified, mirrored and lease-free, so nothing here can hurt
/// it. The outcome split is [`site_hook_outcome`]:
///
/// - exit 0 — synced, or the hook's own deliberate deferrals (no Firebase
///   login: "SITE NOT DEPLOYED — committed and pushed; deploy later with
///   deploy.sh"), which its transcript already narrates;
/// - exit 3 — this machine has NO site checkout. Structural: no `--resume` on
///   this machine can ever complete the step, and parking the journal would
///   block the next cut behind a checkout that does not exist here. Announced
///   LOUDLY (with the exact command for a machine that has the checkout) and
///   marked done;
/// - exit 4 — deployed, but the live site still lags after the settle loop
///   (CDN). The deploy itself succeeded; announced, marked done, re-check by
///   hand;
/// - anything else — a real failure. The step FAILS, naming the release as
///   safe, and the journal parks at "site": `cargo ship cut --resume` re-enters
///   exactly here (the hook is idempotent — an already-synced site is "nothing
///   to commit"), and `publish/post-promote --latest` is the same retry without
///   the journal.
fn step_site(ctx: &mut CutCtx) -> Result<()> {
    let hook = ctx.repo.join("publish/post-promote");
    if !hook.is_file() {
        step(
            "site",
            "publish/post-promote is not in this tree — no public website follows this channel; skipped",
        );
        return Ok(());
    }
    step(
        "site",
        &format!(
            "alab.systems follows the cut: publish/post-promote --latest \
             (download button \u{2192} aterm-{}.dmg on the public channel)",
            ctx.version
        ),
    );
    let status = Command::new(&hook)
        .arg("--latest")
        .env("PUB_VERSION", &ctx.version)
        .current_dir(&ctx.repo)
        .status()
        .map_err(|error| {
            Error::new(format!(
                "cannot run {}: {error}; the release v{} is LIVE, verified and mirrored — only \
                 the website step is owed. Retry with `cargo ship cut --resume`, or run \
                 `publish/post-promote --latest` by hand",
                hook.display(),
                ctx.version
            ))
        })?;
    match site_hook_outcome(status.code()) {
        SiteHookOutcome::Synced => {
            step("site", "alab.systems synced (or deferred with instructions above)");
            Ok(())
        }
        SiteHookOutcome::NoSiteCheckout => {
            // No site checkout on this machine — post-promote touched nothing.
            step(
                "site",
                &format!(
                    "WARNING: NO SITE CHECKOUT ON THIS MACHINE — alab.systems still links the \
                     PREVIOUS release's DMG. The cut is complete and unaffected; from a machine \
                     with the site checkout run: publish/post-promote --latest   (set SITE_DIR \
                     if it is not at ~/company-life/companies/ferrite/workspace-alab); v{} will \
                     then be the download",
                    ctx.version
                ),
            );
            Ok(())
        }
        SiteHookOutcome::LiveLagging => {
            step(
                "site",
                "deployed, but the live site still served the old bytes after the settle loop \
                 (CDN lag or a concurrent deploy) — re-check https://alab.systems in a minute",
            );
            Ok(())
        }
        SiteHookOutcome::Failed => Err(Error::new(format!(
            "publish/post-promote --latest failed ({}); the release v{} is LIVE, verified and \
             mirrored — the cut is safe, only the website step is owed. The journal parks at \
             \"site\": retry with `cargo ship cut --resume` (re-enters exactly here), or run \
             `publish/post-promote --latest` by hand and then `cargo ship cut --resume` to \
             converge the journal (an already-synced site is \"nothing to commit\")",
            status
                .code()
                .map_or_else(|| "killed by signal".to_string(), |c| format!("exit {c}")),
            ctx.version
        ))),
    }
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

// ---------------------------------------------------------------------------
// Notarize + package: the ordered middle of `step_build`, extracted so the
// ORDER is a tested fact
// ---------------------------------------------------------------------------

/// The two container builders and the two digest reads, behind a trait.
///
/// `hdiutil` and `ditto` want a real signed bundle and tens of seconds; the
/// sequence that calls them is where a mutation is invisible and expensive —
/// notarizing after the zip is built, or skipping the post-hook re-hash, both
/// produce a green cut and a broken artifact. This seam is what lets
/// [`notarize_and_package`] be driven end to end, offline, by a fake that records
/// what happened in what order.
///
/// It wraps `dmg.rs` rather than changing it: the real implementation is four
/// one-line delegations, so nothing about how a DMG is actually built moved.
///
/// RETIRED 2026-08-26: the `dmg_arch` (Intel `-x86_64` restage) and `dmg_lite`
/// (seed-stripped `-lite` twin) lanes, and the `notarized`/`seeded` facts the
/// zip lane took to judge its restage — there is no seed and no restage; the
/// zip archives the bundle exactly as the DMG images it.
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
    /// The DMG digest AFTER the Tier APPLE hook, which rewrites the bytes.
    pub dmg_sha256: String,
    /// The DMG size AFTER the hook, for the same reason.
    pub dmg_size: u64,
    pub zip: dmg::Packaged,
}

/// Notarize the bundle, build both containers around it (the DMG, then the
/// zip), notarize the DMG, and return the digests that go on record in the
/// manifest.
///
/// # The order is the property
///
/// Every line of this function is sequenced against a failure that produces a
/// GREEN cut and a broken artifact, which is why it is one extracted unit with
/// one test over it rather than five statements inline in a 300-line step:
///
/// 1. the `.app` is notarized and stapled FIRST, because [`Packager::zip`]
///    archives the bundle as it finds it and the zip is what every self-updating
///    install downloads — a zip made before the staple strands the fleet on any
///    Mac that cannot reach Apple;
/// 2. the DMG is built AFTER that staple, so the human's artifact carries the
///    ticket twice over;
/// 3. the DMG is Developer-ID signed and notarized by the hook, which REWRITES
///    its bytes;
/// 4. so the DMG digest is re-read from disk afterwards — driven by what the
///    hook REPORTS doing, so the re-hash cannot drift away from the mutation it
///    exists to cover.
///
/// On the inactive tier (the shipped one) both hooks do nothing, no re-hash
/// happens, and this is `dmg::create` + `dmg::create_zip` with the digests they
/// minted — byte-for-byte the pipeline as it has always run.
pub fn notarize_and_package(
    app: &Path,
    dist: &Path,
    version: &str,
    tier: &sign::AppleTier,
    tools: &dyn sign::AppleTools,
    pack: &dyn Packager,
) -> Result<PackagedCut> {
    // Said BEFORE the two notarization waits, and said HERE rather than in `sign.rs`,
    // which deliberately references nothing else in the crate so `tests/signconf.rs` can
    // mount it alone.
    //
    // The crate teaches, in the ONE other place it mentions interrupting — the
    // certificate wait, "Ctrl-C is safe, nothing is lost and this step resumes" — that a
    // long silent wait may be interrupted. This is the other kind, and every fact below
    // already lived in `run_streamed`'s doc and in `NOTARY_SUBMIT_TIMEOUT` without ever
    // being printed.
    if tier.identity().is_some() {
        step(
            "notarize",
            &format!(
                "Apple decides how long this takes, not us: usually 2-10 min, and this cut \
                 gives up after {} min.\n\
                 ⚠ do NOT Ctrl-C — unlike the certificate wait, this cut is holding a \
                 release lease, a publisher fence and a burned build number, and abandoning \
                 it here is recoverable only through an explicit killed-machine takeover \
                 (`cargo ship recover`).\n\
                 notarytool streams its own progress below.",
                sign::NOTARY_SUBMIT_TIMEOUT.as_secs() / 60
            ),
        );
    }
    // THE BUNDLE IS NOTARIZED FIRST, before either container exists.
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

    // THE ONE DMG: `dmg::create`'s image of the app exactly as signed (and, on
    // the active tier, stapled), under the fleet-pinned bare `aterm-<v>.dmg`
    // name.
    let dmg_out = pack.dmg(app, dist, version)?;
    // THE hook. Inactive: returns false having done nothing. Active: Dev-ID
    // signs, preflights, notarizes and staples the DMG — and any failure in that
    // sequence propagates here and aborts the cut, because the manifest stamps
    // `team_id` from the anchor unconditionally and a non-empty `team_id` is a
    // promise to `tools/install.sh` and the in-app updater that the artifact is
    // notarized. There is no state in which we make that claim without having
    // earned it.
    let dmg_notarized =
        sign::sign_and_notarize_dmg(&dmg_out.path, tier, tools).map_err(Error::new)?;
    // Re-hash AFTER the hook: codesign REWRITES the DMG bytes and the staple
    // appends a ticket, so the digest `dmg::create` minted covers the pre-hook
    // bytes only. The manifest sha256 must be the digest of the exact bytes
    // clients download — a stale one would hard-abort the self-check after the
    // whole build+notarize, and (were the self-check ever skipped) fail the
    // sha256 gate on every v0.25 client.
    let (dmg_sha256, dmg_size) = if dmg_notarized {
        (pack.sha256(&dmg_out.path)?, pack.size(&dmg_out.path)?)
    } else {
        (dmg_out.sha256.clone(), dmg_out.size_bytes)
    };
    // The updater container, from the SAME signed — and, on the active tier,
    // already stapled — .app. It is built from the bundle rather than from the
    // DMG because `ditto` must archive the bundle directly to preserve its seal,
    // and `create_zip` hashes what it writes, so its digest already covers the
    // ticket without a second pass.
    let zip = pack.zip(app, dist, version)?;
    Ok(PackagedCut {
        dmg: dmg_out,
        dmg_sha256,
        dmg_size,
        zip,
    })
}

/// Step "build": per-arch builds → lipo → dSYM → bundle → sign → DMG →
/// notarize hook → provenance → manifest + notes. One re-enterable unit whose
/// outputs are all functions of (version, build_number, claim commit).
fn step_build(ctx: &mut CutCtx) -> Result<()> {
    // No ambient credentials, and no trust anchors injected into the child build.
    // Both anchors are committed constants (`aterm_update_core::pins`) that the
    // child compiles in directly, so exporting them here would only create a second
    // source that could disagree with the first — the exact bug 068a6e2c removed.

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
        expected_update_pin_sha256: ctx.expected_embedded_pin()?,
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

    let spec = bundle::BundleSpec {
        repo_root: ctx.repo.clone(),
        out_dir: ctx.dist.clone(),
        short_version: ctx.version.clone(),
        build_number: ctx.build,
        bundle_id: "com.aterm.aterm".to_string(),
        git_commit: stamp.clone(),
        aterm_bin: bout.aterm,
    };
    let app = bundle::assemble(&spec)?;
    step(
        "bundle",
        &format!(
            "aterm.app: Short={}  CFBundleVersion={}  ATermGitCommit={stamp}  lean (the \
             toolchain self-provisions on first launch)",
            ctx.version, ctx.build,
        ),
    );

    // Tier APPLE, resolved once at the entry point. Inactive (the shipped tier)
    // means `identity()` is None and every hook below is a no-op, leaving this
    // region byte-for-byte the ad-hoc path it has always been.
    let sign_id = ctx.apple.identity();
    let signed_by = sign::sign_app(
        &app,
        &ctx.repo.join("apps/aterm-mac/aterm.entitlements"),
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

    // Notarize the bundle, package both containers around it, notarize the DMG,
    // and re-hash it — ONE ordered unit, because every step of that order is
    // load-bearing and none of it is observable from a green cut. See
    // `notarize_and_package`; its ordering and its fail-closed propagation are
    // proved offline in tests/apple_tier.rs.
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
    // The DMG must clear the client's own download bound before anything is
    // hashed into a manifest: a cut that packages past it publishes a release
    // no client can download. (The seeded dual-arch image once reached 97.3%
    // of the 2 GiB `RELEASE_ASSET_DOWNLOAD_BOUND`; the lean image is ~28 MB,
    // and the check stays because the bound is the client's, not ours.)
    validate_release_asset_download_size(dmg_size)?;
    // The stable download twins are copied only HERE, after
    // `notarize_and_package` has produced the FINAL container bytes
    // (codesign/staple rewrites included), so each twin is byte-identical to
    // the bytes its in-process digest covers. required_asset_names() lists
    // every one of them, so the mirror uploads them and refuses a channel head
    // without them. The twins are the PERMANENT download names the README
    // publishes and readers bookmark. (They are no longer what alab.systems
    // links: since 2026-08-28 the site's button is the VERSIONED
    // `aterm-<v>.dmg`, rewritten on every promote by `publish/post-promote`,
    // so the button downloads exactly the file it names.) The DMG twin `aterm.dmg` is a
    // byte copy of manifest.dmg — the ONE lean DMG (RETIRED 2026-08-26: the
    // `-lite` twin it used to alias, and the `aterm-offline.dmg` alias of the
    // seeded image).
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
    // Provenance AFTER signing: binary_sha256 must cover the SIGNED bytes.
    let provenance_path = bundle::write_provenance(&spec, &app, &signed_by)?;
    if ctx.signature_required {
        // Bind the provenance to the fingerprint the BINARY carries — the committed
        // keyset head, which is what `aterm-gui/build.rs` embeds — not to the signing
        // key's. The field records which anchor reached the artifact, so recording a
        // fingerprint the artifact does not contain would make the record a claim about
        // the machine instead of about the build. The two are the same string on every
        // configuration that exists today (see `expected_embedded_update_pin`), and
        // stop being the same the moment a rostered non-head machine cuts, which is
        // exactly when a self-consistent record matters.
        let fingerprint = ctx
            .expected_embedded_pin()?
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
    // `.sha256` sidecars for the containers AND their stable twins, from the
    // SAME in-process digests that feed the manifest — `shasum -a 256 -c`
    // records, exactly like the Linux tarball's. The containers are the manual
    // downloads and their digests otherwise live only inside the appcast TOML
    // no human opens; these ~99-byte assets are what the release notes' verify
    // instruction points at. The twin sidecars restate the twin's digest under
    // the ALIAS filename — never a rehash of a separate artifact, because each
    // twin is a byte copy of the exact bytes its digest here covers — since a
    // `shasum -c` record names the file it checks, and the versioned sidecar
    // can never verify the file a `releases/latest/download/...` click
    // actually saves.
    let sidecars = [
        (
            ctx.dmg_sha256_path(),
            dmg_sha.as_str(),
            mirror::dmg_asset_name(&ctx.version),
        ),
        (
            ctx.zip_sha256_path(),
            zout.sha256.as_str(),
            mirror::zip_asset_name(&ctx.version),
        ),
        // The alias sidecars restate their SOURCE artifact's digest under the
        // alias filename — each twin above is a byte copy of exactly the
        // artifact whose digest it restates.
        (
            ctx.stable_dmg_sha256_path(),
            dmg_sha.as_str(),
            mirror::stable_dmg_asset_name(),
        ),
        (
            ctx.stable_zip_sha256_path(),
            zout.sha256.as_str(),
            mirror::stable_zip_asset_name(),
        ),
    ];
    for (path, sha, name) in sidecars {
        fs::write(&path, mirror::sha256_sidecar_contents(sha, &name))
            .map_err(|e| Error::new(format!("write {}: {e}", path.display())))?;
    }
    step(
        "",
        "`.sha256` sidecars staged for every container and every stable twin \
         (shasum -a 256 -c)",
    );
    // ---- manifest + notes (the rolled body, verbatim, once — spec §3) -----
    let cl_text = fs::read_to_string(ctx.repo.join(changelog::CHANGELOG_FILE))
        .map_err(|e| Error::new(format!("read {}: {e}", changelog::CHANGELOG_FILE)))?;
    let body = changelog::rolled_body(&cl_text, &ctx.notes_section)?;
    // The GITHUB body gets the standing newcomer preamble; the manifest's
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
        dmg_name: &mirror::dmg_asset_name(&ctx.version),
        dmg_sha256: &dmg_sha,
        zip_name: &mirror::zip_asset_name(&ctx.version),
        // No re-hash pass needed, but not because nothing touches the zip — on
        // the active tier the bundle inside it carries a notarization ticket.
        // `create_zip` runs AFTER the staple and hashes the bytes it writes, so
        // this digest already covers them.
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
        team_id: aterm_update_core::pins::APPLE_TEAM_ID,
        pub_date: &bundle::epoch_to_rfc3339(unix_now()),
        min_build: ctx.min_build,
        changelog: &body,
    };
    let mpath = stage_manifest(&ctx.dist, &inputs, ctx.attribution.as_ref())?;
    // The roster assets are staged from the bytes the PRE-CLAIM gate authorized, and
    // staged BEFORE the signature below for no cryptographic reason at all — they are
    // separately master-signed and the appcast signature does not cover them. It is
    // ordering for the operator's sake: if this fails, it fails before a signature
    // exists to be confusing about.
    stage_roster_assets(&ctx.dist, ctx.roster.as_ref())?;
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
        journal.verify_pubkey.clone_from(&ctx.verify_pubkey);
        journal
            .signature_machine_id
            .clone_from(&ctx.signature_machine_id);
    }
    Ok(())
}
/// The SIGNING half of the self-check: the hard `codesign` gate every cut faces,
/// and the Tier APPLE evidence a cut faces iff its manifest CLAIMS a team.
///
/// Returns the transcript suffix the "selfcheck" step prints, which is how the
/// transcript's Tier APPLE claim is made BY this verdict rather than beside it:
/// there is no arrangement of this code in which the line says "stapled ticket +
/// Gatekeeper" without these checks having passed, because the words are this
/// function's return value.
///
/// # Why the gate lives here, and why the evidence comes through the seam
///
/// This branch is the invariant, not the happy path. A resumed cut skips
/// [`step_build`] entirely when the journal marks it done, so this is the only
/// thing that re-proves the artifacts on disk match what the manifest says about
/// them. It is gated on the MANIFEST's `team_id` rather than on `ctx.apple`
/// deliberately: the manifest is the promise that ships, and re-deriving the
/// tier here would let a cut be judged by what the cutting machine can do today
/// instead of by what its own artifact claims.
///
/// Every spawn goes through [`sign::AppleTools`] — including the plain
/// `codesign --verify --deep --strict`, whose verdict decides whether a release
/// ships and which therefore has no business being resolved through `$PATH`
/// (see [`sign::RealAppleTools`], where each tool is named absolutely once).
/// Routing it through the seam is also what makes this whole branch, gate
/// included, reachable from a test with no certificate and no Apple account.
pub fn selfcheck_signing(
    team: &str,
    app: &Path,
    dmg: &Path,
    tools: &dyn sign::AppleTools,
) -> Result<&'static str> {
    // The hard gate (sign.rs's inline verify print is advisory), on EVERY tier
    // including the ad-hoc one that ships today.
    tools.codesign_verify_strict(app).map_err(|e| {
        Error::new(format!(
            "self-check failed: codesign --verify --deep --strict: {e}"
        ))
    })?;
    if team.is_empty() {
        // The shipped tier claims no team, so there is no notarization promise
        // to keep and nothing further to prove. Note what is NOT done here: no
        // Apple tool is spawned at all, so an inactive cut costs exactly what it
        // did before Tier APPLE was wired.
        return Ok("");
    }
    // Evidence gathered here, verdict passed in sign.rs — so the rules are
    // testable without an Apple account, and so this function has nothing to
    // decide beyond WHICH evidence to collect.
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

// ---------------------------------------------------------------------------
// The cut's PAINT SMOKE — ten keystrokes against the just-built bundle
// (2026-08-24 blackout audit, docs/RELEASE-PROOF-DISCIPLINE.md)
// ---------------------------------------------------------------------------

/// `--no-paint-smoke`: the emergency escape from the self-check's paint smoke.
///
/// An ESCAPE, not a setting — v0.48.0 and v0.49.0 shipped the rainbow cursor
/// trail dark past green gates precisely because no gate ever looked at a
/// shipped artifact's pixels, so the smoke this flag skips is the one check
/// standing between "the pipeline confirms" and "the feature works". On a
/// notarized real cut it is refused outright unless the operator ALSO sets
/// [`NO_PAINT_SMOKE_ACK_VAR`] to [`NO_PAINT_SMOKE_ACK_VALUE`] — an
/// acknowledgement whose spelling says what is being accepted.
pub const NO_PAINT_SMOKE_FLAG: &str = "--no-paint-smoke";
/// The env acknowledgement `--no-paint-smoke` requires on a notarized real cut.
pub const NO_PAINT_SMOKE_ACK_VAR: &str = "ATERM_NO_PAINT_SMOKE_ACK";
/// The exact required value — strict, like every env switch here: a value that
/// names the risk cannot be set by accident, and `=0` never means "yes".
pub const NO_PAINT_SMOKE_ACK_VALUE: &str = "this-cut-may-ship-dark";

/// The paint probe seam: launch the just-built bundle's binary headless, drive
/// the fake-Claude shape with real keystrokes over its own control socket,
/// record ~3s of pixels, and scan for effect ink.
///
/// A trait for exactly the reason [`Packager`] and [`sign::AppleTools`] are:
/// the real probe launches a GUI process and records video, which no unit test
/// can afford, and the sequence that calls it is where a mutation is invisible
/// and expensive — `if false`-ing the call site ships the next dark release.
/// The recording fakes in tests/paint_smoke.rs drive the real decision code
/// and assert what it DID, in what ORDER.
pub trait PaintProbe {
    /// `Ok(report)` = the effect painted (the probe's one-line measurement,
    /// which the transcript prints so the claim carries its evidence).
    /// `Err(why)` = it did not, or nothing could be proven — the CALLER owns
    /// the verdict words; this is only the measurement.
    fn paint(&self, bundle_binary: &Path) -> std::result::Result<String, String>;
}

/// The real probe: `tools/paint-conformance/paint_probe.sh` — the SAME driver
/// and scanner the CI paint-conformance matrix runs, so the cut's smoke and
/// the matrix cannot drift apart. Budgeted at ~20s inside the script itself
/// (watchdog included): a paint proof that can hang is a gate nobody runs.
pub struct RealPaintProbe {
    pub repo: PathBuf,
}

impl PaintProbe for RealPaintProbe {
    fn paint(&self, bundle_binary: &Path) -> std::result::Result<String, String> {
        let script = self.repo.join("tools/paint-conformance/paint_probe.sh");
        if !script.is_file() {
            return Err(format!(
                "paint probe missing ({}) — nothing was proven about paint",
                script.display()
            ));
        }
        let out = Command::new(&script)
            .arg(bundle_binary)
            .args(["--shape", "fake-claude"])
            .args(["--keys", "r,a,i,n,b,o,w,space,o,n"])
            // UNPINNED, UNFOCUSED — the only configuration that can catch the
            // failure this smoke exists for. `--capture video` drives
            // `ctl video`, and an in-flight recording PINS `App::motion_focus`
            // for the recorded window: the gate would un-suppress the very
            // motion demotion that blacked out the trail in v0.48, v0.49 and
            // v0.50, and pass all three. `--capture image` leaves the gate
            // exactly as the un-observed app has it, and `--focus out` puts the
            // window in the state the owner's real windows are in (typed into
            // without OS key focus — control-socket input and handoff-adopted
            // windows never hold it).
            .args(["--capture", "image", "--focus", "out"])
            .args(["--record", "3", "--expect", "ink", "--budget", "25"])
            .output()
            .map_err(|e| format!("could not spawn {}: {e}", script.display()))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let report = stdout
            .lines()
            .rev()
            .find(|l| l.starts_with("PAINT"))
            .unwrap_or("<no PAINT report line>")
            .to_string();
        match out.status.code() {
            Some(0) => Ok(report),
            // 1 = the expectation failed, 2 = could not run; both refuse the
            // cut (could-not-run is NOT a pass — the exact vacuity the audit
            // found), under words the caller composes.
            Some(1 | 2) => Err(report),
            code => Err(format!(
                "paint probe died abnormally (exit {code:?}; the protocol is 0/1/2): {report}"
            )),
        }
    }
}

/// The `--no-paint-smoke` policy: `Ok(None)` = the smoke runs; `Ok(Some(words))`
/// = skipped, with the transcript line that says so out loud; `Err` = the skip
/// is REFUSED (a notarized real cut without the explicit acknowledgement).
///
/// Pure and separated from the probe so tests/paint_smoke.rs can drive every
/// arm without launching anything.
pub fn paint_smoke_policy(
    kind: CutKind,
    notarized_claim: bool,
    skip_requested: bool,
    ack: Option<&str>,
) -> Result<Option<String>> {
    if !skip_requested {
        return Ok(None);
    }
    if kind == CutKind::Real && notarized_claim {
        if ack != Some(NO_PAINT_SMOKE_ACK_VALUE) {
            return Err(Error::new(format!(
                "{NO_PAINT_SMOKE_FLAG} on a notarized real cut is refused: this is the check \
                 that would have stopped v0.48.0/v0.49.0 shipping the rainbow trail dark \
                 (docs/RELEASE-PROOF-DISCIPLINE.md). If this is a genuine emergency, say so \
                 explicitly: {NO_PAINT_SMOKE_ACK_VAR}={NO_PAINT_SMOKE_ACK_VALUE}"
            )));
        }
        return Ok(Some(format!(
            "SKIPPED by {NO_PAINT_SMOKE_FLAG} + {NO_PAINT_SMOKE_ACK_VAR} — this notarized cut \
             ships with NO pixel proof of its flagship effect"
        )));
    }
    Ok(Some(format!(
        "SKIPPED by {NO_PAINT_SMOKE_FLAG} — this cut ships with NO pixel proof of its \
         flagship effect"
    )))
}

/// The self-check's two artifact probes, IN ORDER: the paint smoke against the
/// just-built bundle FIRST, the codesign/Tier-APPLE gate second.
///
/// One extracted unit for the same reason [`notarize_and_package`] is one: the
/// ORDER is the property. The smoke must judge the bundle before the signing
/// verdict is pronounced — a cut that fails to paint must die without spending
/// a single Apple tool spawn, and no transcript may carry the signing claim
/// for an artifact whose flagship effect was never seen to paint. Both notes
/// are these functions' RETURN VALUES, so neither claim can be printed without
/// its check having passed (the [`selfcheck_signing`] rule, extended).
///
/// tests/paint_smoke.rs drives this with recording fakes across both seams and
/// fails under exactly the mutations that would resurrect the blackout:
/// `if false`-ing the probe call, reordering it after the signing gate, or
/// downgrading a probe failure to a warning.
#[allow(clippy::too_many_arguments)]
pub fn selfcheck_paint_then_signing(
    kind: CutKind,
    team: &str,
    app: &Path,
    dmg: &Path,
    skip_requested: bool,
    ack: Option<&str>,
    probe: &dyn PaintProbe,
    tools: &dyn sign::AppleTools,
) -> Result<(String, &'static str)> {
    let paint_note = match paint_smoke_policy(kind, !team.is_empty(), skip_requested, ack)? {
        Some(skip_words) => skip_words,
        None => match probe.paint(&app.join("Contents/MacOS/aterm")) {
            Ok(report) => format!("10 keys, fake-Claude shape, ink asserted \u{2014} {report}"),
            Err(why) => {
                return Err(Error::new(format!(
                    "self-check failed: the shipped artifact does not paint its flagship \
                     effect \u{2014} see docs/RELEASE-PROOF-DISCIPLINE.md ({why})"
                )));
            }
        },
    };
    let apple_note = selfcheck_signing(team, app, dmg, tools)?;
    Ok((paint_note, apple_note))
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
        // Prove the shipped binary embedded the pin the BUILD expected, and that the
        // provenance records the same one — through the SAME accessor `step_build`
        // used, so the two can never state different expectations of one artifact.
        //
        // Deriving this from `ctx.signature_pubkey` instead was the same long-fuse trap
        // `expected_embedded_update_pin` exists to close, relocated one step later: on
        // the armed path a rostered non-head machine would clear every pre-claim gate,
        // burn a ledger number, build and notarize for the better part of an hour, and
        // then fail here with a fingerprint mismatch naming neither the roster nor the
        // keyset. Identical on every configuration that exists today.
        let fingerprint = ctx
            .expected_embedded_pin()?
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

    // RETIRED 2026-08-26: the Intel DMG pair and the `-lite` twin with its
    // journaled digest record. The manifest names ONE DMG, proved above; a
    // manifest that still names an Intel variant was not staged by this
    // cutter.
    refuse_retired_intel_dmg(&manifest)?;

    // The stable download twins are byte copies of containers already proved
    // against the manifest's digest records, and dist/ is mutable while a
    // resume skips `step_build`, so the copies are re-proved too: the
    // mirror later verifies the public channel's alias objects against dist/'s
    // twins, never against the canonical containers, so a stale twin here
    // would cross byte-verified against itself. The digest is the record's
    // own — the twin is only ever a byte copy, so hashing it and comparing IS
    // the identity proof, never an independent record. A twin MISSING outright
    // is a pre-twin journal's resume shape and is regenerated from the proven
    // canonical bytes (same reasoning as the sidecars below); a twin PRESENT
    // with different bytes is refused, not repaired — this cutter only writes
    // byte copies, so something else wrote it. (A journal from the retired
    // lite lane, resumed here, has an `aterm.dmg` that aliases the OLD lean
    // twin's bytes, not manifest.dmg's — that is exactly the divergent case
    // this refuses; a human re-cuts rather than the mirror serving two
    // contracts under one evergreen name.)
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

    // The `.sha256` sidecars on disk must state EXACTLY the digests the manifest
    // does — dist/ is mutable and a resume skips `step_build`, so a stale sidecar
    // from an earlier attempt would ship a verification record that fails against
    // the very bytes beside it. Byte equality against the regenerated record, not
    // a parse: the sidecar has one legal spelling (`<hash>  <name>\n`).
    //
    // A sidecar that is MISSING outright is the other resume shape: a cut staged
    // by a pre-sidecar cutter, resumed by this one. Sidecars are pure functions
    // of digests the manifest already binds, so they are regenerated here the
    // same way `recover_published_cut` reconstructs them for old releases —
    // refusing would strand every journal written before sidecars existed. The
    // twins' ALIAS sidecars ride the same rule: same digests, alias filenames
    // (each names the exact bytes its evergreen URL saves).
    let stable_dmg_name = mirror::stable_dmg_asset_name();
    let stable_zip_name = mirror::stable_zip_asset_name();
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
        let expected = mirror::sha256_sidecar_contents(sha, name);
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

    // The paint smoke, then codesign + Tier APPLE (spec §7 step 4, the tier
    // iff the manifest CLAIMS a team) — one ordered unit, so the bundle is seen
    // to PAINT before any signing verdict is pronounced and before any
    // publish-facing step runs. Each suffix is its own verdict's words — see
    // `selfcheck_paint_then_signing` / `selfcheck_signing`.
    let team = manifest.team_id.clone().unwrap_or_default();
    let ack = std::env::var(NO_PAINT_SMOKE_ACK_VAR).ok();
    let (paint_note, apple_note) = selfcheck_paint_then_signing(
        ctx.kind,
        &team,
        &app,
        &ctx.dmg_path(),
        ctx.no_paint_smoke,
        ack.as_deref(),
        &RealPaintProbe {
            repo: ctx.repo.clone(),
        },
        &sign::RealAppleTools,
    )?;
    step("paint", &paint_note);
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

/// Replay the client selection against the publish target and apply both the
/// monotonic-build gate and the carried-floor gate; returns the selected live
/// build for the transcript.
/// The `roster_seq` a published channel head was cut under, read out of its own
/// manifest bytes.
///
/// `verify::Published` already carries the exact downloaded manifest text, so the
/// generation the channel is standing on costs a parse rather than a fetch. `None`
/// means the head carries no attribution — an unarmed channel, which is every channel
/// this tree publishes to.
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

/// The `roster_seq` THIS cut will publish, from whichever of its two authorities is
/// available at the moment of asking.
///
/// Before `build` the authority is the pre-claim gate's attribution. After `build` it
/// is the staged manifest itself — which is the stronger of the two, because those are
/// the bytes that will actually ship, and a resume past `build` deliberately carries no
/// attribution to re-stamp with.
///
/// A cut that attaches a roster but can answer neither is an ERROR rather than a silent
/// `None`: `None` reads as "no roster in play" to [`roster_floor_covered`], which would
/// turn an unreadable manifest into a passed ratchet.
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

fn best_published(ctx: &CutCtx) -> Result<Option<u64>> {
    let scanned = if ctx.kind == CutKind::Rehearse {
        verify::scan_published(&ctx.slug, true)?
    } else {
        verify::scan_published_in_repo(&ctx.repo, &ctx.slug, true)?
    };
    let best = scanned.first();
    let newest_floor = best.and_then(|published| published.min_build);
    // The roster ratchet rides with the `min_build` ratchet, at all four of the places
    // that guard it — lock, selfcheck, preflip, flip — because it closes the same race
    // for the same reason: another publisher's release can land between this cut's
    // pre-claim scan and its flip, and a channel head is never allowed to become
    // visible under a roster generation the fleet has already moved past.
    //
    // BOTH numbers, as at pre-claim: the head MANIFEST's attribution and the roster
    // ASSET the public channel actually serves. A machine joining the roster attaches
    // the new pair to already-published releases WITHOUT re-signing their manifests,
    // and every client ratchets on the asset it observed — so a join that lands
    // between this cut's pre-claim and its flip moves the fleet's floor while the
    // manifest number stays put. Reading only the manifest here let such a cut flip
    // under the old generation and strand every client that had ratcheted (2026-08-19
    // audit). A public channel that cannot be read fails closed: a wrong answer here
    // burns a build number and strands the fleet.
    let manifest_roster_seq = published_roster_seq(best)?;
    let observed_roster = match (&ctx.mirror_slug, ctx.kind) {
        (Some(slug), CutKind::Real) if *slug != ctx.slug => machines::channel_roster_document(slug)
            .map_err(|e| {
                Error::new(format!(
                    "cannot read the machine roster on the public channel {slug}'s latest \
                     release ({e}); refusing to reason about the fleet's roster floor"
                ))
            })?,
        _ => None,
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
    let payload = aterm_json::to_vec(&aterm_json::json!({
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
    // The roster travels with the release it authorizes; see `upload_asset_paths`. The
    // predicate is the JOURNALED machine id, not the in-memory roster document, because
    // a resume past `build` has the assets on disk and no document in hand — keying off
    // the document would silently stop attaching them.
    let mut files: Vec<PathBuf> = ctx.upload_asset_paths();
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
    // BEFORE ANY REMOTE PROBE. This verdict is local and provable, and the probe
    // below is a network call that can fail — under exactly the conditions that
    // produce a curl exit 2 in the first place (memory pressure kills the gh spawn;
    // a hammered API answers 5xx three times). A failed probe used to return early
    // and leave the intent standing, restoring the permanent wedge this retraction
    // exists to prevent (2026-08-19 round-7 audit). The pre-POST probe above already
    // answered "did this asset exist beforehand".
    if transport_never_started(&out) {
        ctx.retract_upload_intent(name)?;
        return Err(Error::new(format!(
            "upload of {name} never reached the network (curl exit {}): {}. The durable \
             intent was retracted, so a resume will retry it once the local cause is fixed",
            out.status,
            out.stderr_utf8().trim()
        )));
    }
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
    let mut names: Vec<String> = inventory_before
        .iter()
        .map(|asset| asset.name.clone())
        .collect();
    // A draft uploaded by a pre-sidecar cutter lacks exactly the `.sha256`
    // sidecar assets. They are pure digest records `step_selfcheck` above just
    // re-proved (regenerating them on disk if absent), and
    // `recover_published_cut` already attaches them to PUBLISHED releases on
    // the same reasoning — so converge the draft here rather than strand every
    // pre-sidecar journal at its own proof step. Anything else missing still
    // fails `validate_draft_asset_set` below, on a fresh listing.
    let sidecar_uploads = [
        (
            mirror::sha256_sidecar_name(&mirror::dmg_asset_name(&ctx.version)),
            ctx.dmg_sha256_path(),
        ),
        (
            mirror::sha256_sidecar_name(&mirror::zip_asset_name(&ctx.version)),
            ctx.zip_sha256_path(),
        ),
    ];
    let mut converged = false;
    for (name, path) in &sidecar_uploads {
        if !names.iter().any(|n| n == name) && path.is_file() {
            upload_release_asset_by_id(ctx, release_id, path)?;
            converged = true;
        }
    }
    if converged {
        names = release_asset_inventory_for_release_id(&ctx.slug, release_id)?
            .iter()
            .map(|asset| asset.name.clone())
            .collect();
    }
    validate_draft_asset_set(
        &names,
        &manifest,
        ctx.signature_required,
        &provenance_name,
        dsym_name,
    )?;

    // `validate_draft_asset_set` above has already required the roster's PRESENCE from
    // the manifest's own `machine_id`; `proof_asset_paths` is the bytes half of that.
    let mut files = ctx.proof_asset_paths();
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
        ctx.verification_pubkey(),
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
    // THE MIRROR IS A COPY OF THE ORIGIN RELEASE, NOT OF dist/. The roster pair in
    // dist/ is the one file a separate, un-lease-gated ceremony (`atpkg-keys join`,
    // `cargo ship provision`) rewrites between an origin flip and a resumed mirror —
    // and this step then uploaded THOSE bytes beside a manifest signed under the
    // roster the cut actually shipped, and judged the lineage fork from them too (a
    // spurious fork no supported command could clear; and, at an equal generation, a
    // genuinely forked document published as the channel head). Bind dist/'s roster
    // to what the origin release carries before either use.
    //
    // READ BEFORE THE CHANNEL CREDENTIAL IS ENTERED: this fetch targets the PRIVATE
    // origin repo, which the release-org channel token cannot read (2026-08-19
    // round-4 skeptics — inside the scope every roster cut would have died here).
    let shipped_roster = ctx
        .attaches_roster()
        .then(|| {
            let origin_release_id = ctx.release_id.ok_or_else(|| {
                Error::new("mirror step reached with no bound origin release ID".to_string())
            })?;
            download_release_asset_for_release_id(
                &ctx.slug,
                origin_release_id,
                roster::ROSTER_ASSET,
            )
        })
        .transpose()?;
    if let Some(shipped) = shipped_roster.as_ref() {
        let local = fs::read(ctx.dist.join(roster::ROSTER_ASSET)).map_err(|e| {
            Error::new(format!(
                "read this cut's roster asset from dist/ before mirroring it: {e}"
            ))
        })?;
        if local != *shipped {
            return Err(Error::new(format!(
                "dist/{} is NOT the roster this cut published on its origin release — a join or \
                 provision rewrote it after the flip. Mirroring it would publish a roster the \
                 signed manifest does not name. Restore the pair this cut shipped (download \
                 {} and its .sig from the {} release into dist/), then `cargo ship cut \
                 --resume`; or retire this cut with `cargo ship cut --retire-unmirrored {}`",
                roster::ROSTER_ASSET,
                roster::ROSTER_ASSET,
                ctx.tag,
                ctx.tag,
            )));
        }
    }
    // EVERYTHING below this line talks to the public channel and nothing else: the
    // asset bytes come from local `dist/` files (proved identical to the origin's
    // above), and the two `ctx.slug` uses above are a message and the equality guard.
    // So the release-org credential is safe to hold for the whole step, and it drops
    // on every exit path including `?`.
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
            // AND THE ANONYMOUS PROOF, for the same reason the flip path runs it —
            // every check above rode the release-org credential. Omitting it here made
            // the probe bypassable by the one action an operator always takes when it
            // fails: `step_mirror` PATCHes the release live, the anonymous probe then
            // fails (mirror still membership-restricted, or the CDN not yet serving
            // the DMG inside the probe's window), so the step returns Err and the
            // journal never marks "mirror". The re-run resolves to ConvergePublished,
            // passes the authenticated head proof, and reports the channel live while
            // an unauthenticated GET of the DMG still 404s — the silent
            // never-updates state this probe was added after v0.8.0 to remove.
            prove_channel_is_anonymously_readable(ctx, &slug)?;
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

    // THE LAST LOOK BEFORE THE FLEET CAN SEE IT. Every earlier ratchet (lock,
    // selfcheck, preflip, flip) ran against the ORIGIN, and a resume can reach this
    // step alone, days later. A roster join is not lease-gated — it re-dresses the
    // public head with a newer generation through a separate tool — so it can land
    // between the origin flip and this one; flipping the mirror under the older
    // generation then strands every client that ratcheted (RosterReject::Rollback,
    // no fallback release). Read the public head's roster asset NOW and refuse.
    let fleet_roster = machines::channel_roster_document(&slug).map_err(|e| {
        Error::new(format!(
            "cannot read the machine roster on the public channel {slug}'s current head \
             ({e}) immediately before the public flip; refusing to flip under an unknown \
             fleet floor"
        ))
    })?;
    let carried = cut_roster_seq(ctx)?;
    // Judged from the roster this cut SHIPPED (proved byte-identical to dist/ above),
    // never from dist/ alone.
    if let Some(shipped) = shipped_roster.as_ref() {
        machines::roster_lineage_agrees(shipped, carried, fleet_roster.as_ref())
            .map_err(Error::new)?;
    }
    roster_floor_covered(carried, fleet_roster.as_ref().map(|(seq, _)| *seq))?;

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
/// The retry budget is deliberately short (about a minute): it exists to outlast
/// GitHub's own eventual consistency after a flip, not a rate-limit window, which
/// is an hour and is reported as such — see [`anon_probe_rate_limited`].
///
/// Credentials are stripped from the child on every attempt: the whole point is to
/// see the channel exactly as an install with no token sees it. See
/// [`ANON_PROBE_ATTEMPTS`] for why retrying is sound.
/// Whether an anonymous probe's failure is GitHub's unauthenticated RATE LIMIT
/// (60 requests/hour per IP) rather than a statement about the channel. `curl -f`
/// collapses every 4xx into exit 22 with the status in its message, so the code is
/// read out of the text — the same shape the client's `download_bytes` uses.
///
/// MEASURED 2026-08-19: a cut from a machine that had spent the hour's anonymous
/// budget failed at the post-flip probe with "the public channel … is NOT readable
/// without a credential" and told the operator to make an already-public repo
/// public. The mirror step is the LAST step of a cut, so the release was live on
/// the origin, the draft was on the channel, and the wrong remedy was the only
/// thing on screen.
#[must_use]
fn anon_probe_rate_limited(out: &std::process::Output) -> bool {
    anon_probe_stderr_is_rate_limit(&String::from_utf8_lossy(&out.stderr))
}

/// The text half of [`anon_probe_rate_limited`], split out so it is testable
/// without a process.
#[must_use]
pub fn anon_probe_stderr_is_rate_limit(stderr: &str) -> bool {
    let Some(idx) = stderr.find("returned error: ") else {
        return false;
    };
    let rest = &stderr[idx + "returned error: ".len()..];
    let code: String = rest.chars().take_while(char::is_ascii_digit).collect();
    matches!(code.as_str(), "403" | "429")
}

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
        // A RATE LIMIT IS NOT A VERDICT ABOUT THE CHANNEL. Every probe here is
        // deliberately anonymous, and GitHub's unauthenticated budget is 60 requests
        // per hour PER IP — a machine that has been checking for updates all day
        // (or a NAT) can exhaust it. Say that, with the remedy that actually works.
        if anon_probe_rate_limited(&out) {
            return Err(Error::new(format!(
                "the anonymous readability probe of {url} was RATE LIMITED by GitHub \
                 ({}). This says nothing about whether {slug} is public — the \
                 unauthenticated budget is 60 requests/hour per IP and this machine has \
                 spent it. The release is live on the origin and its channel draft is \
                 uploaded; wait for the hour to roll over (`curl -s \
                 https://api.github.com/rate_limit` shows the reset) and run \
                 `cargo ship cut --resume`, which converges without re-uploading \
                 anything.",
                String::from_utf8_lossy(&out.stderr).trim(),
            )));
        }
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
    for name in
        mirror::required_asset_names(&ctx.version, ctx.signature_required, ctx.attaches_roster())
    {
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
    let payload = aterm_json::to_vec(&aterm_json::json!({
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
    // Local, provable, and evaluated before the network probe — same ordering rule
    // as the private leg, for the same reason.
    if transport_never_started(&out) {
        ctx.retract_mirror_upload_intent(name)?;
        return Err(Error::new(format!(
            "mirror upload of {name} never reached the network (curl exit {}): {}. The \
             durable intent was retracted, so a resume will retry it once the local cause \
             is fixed",
            out.status,
            out.stderr_utf8().trim()
        )));
    }
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
    mirror::validate_mirror_asset_set(
        &names,
        &ctx.version,
        ctx.signature_required,
        ctx.attaches_roster(),
    )?;
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
    mirror::validate_mirror_asset_set(
        &names,
        &ctx.version,
        ctx.signature_required,
        ctx.attaches_roster(),
    )?;

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
            pubkey: ctx.verification_pubkey(),
            local_signature: Some(&signature),
        },
    )
}

#[cfg(test)]
mod transport_body_tests {
    //! THE ASSET LEG'S MEMORY COST. `--data-binary @file` buffers the whole
    //! payload before the socket opens; the batteries-included DMG is over a
    //! gigabyte, so the first seeded cut died with `curl: option --data-binary:
    //! out of memory` AFTER building, signing and notarizing both containers —
    //! the most expensive possible place to learn it. These assert the argv
    //! itself, because that is the only artifact of the decision.

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
    }

    /// The retraction must be decided from the LOCAL result, before any network
    /// probe — a probe that fails would otherwise return early and leave the intent
    /// standing, which is the permanent wedge the retraction exists to prevent. This
    /// asserts the ordering as source, because there is nothing else to observe: the
    /// probe needs a live GitHub release (2026-08-19 round-7 audit).
    #[test]
    fn the_retraction_is_decided_before_any_remote_probe() {
        let src = include_str!("publish.rs");
        for (func, retract) in [
            (
                "fn upload_release_asset_by_id",
                "ctx.retract_upload_intent(name)?",
            ),
            (
                "fn upload_mirror_asset",
                "ctx.retract_mirror_upload_intent(name)?",
            ),
        ] {
            let body = &src[src.find(func).expect("function present")..];
            let issue = body.find("post.issue(permit)?").expect("the POST");
            let retracted = body[issue..].find(retract).expect("the retraction");
            let probed = body[issue..]
                .find("release_asset_identity_for_release_id_optional")
                .expect("the visibility probe");
            assert!(
                retracted < probed,
                "{func}: the local never-sent verdict must precede the remote probe"
            );
        }
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

    /// A retracted intent has to put the pipeline back where it was BEFORE the
    /// permit was minted, or the retraction is cosmetic: the next resume has to
    /// choose "persist and post", not "await visibility" forever.
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

    #[test]
    fn a_json_body_still_buffers_from_its_private_temp_file() {
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
}

#[cfg(test)]
mod roster_wiring_tests {
    //! THE PRODUCER-SIDE ATTACH PATH — the lines that decide whether an armed cut is
    //! attributed and carries its roster AT ALL.
    //!
    //! These live inside `publish.rs` rather than in `tests/machine_roster.rs` because
    //! every property below is a private method of [`CutCtx`], and every one of them
    //! failed silently: a regression on any of them produces a WELL-FORMED release with
    //! no `machine_id` or no `aterm-machines.toml`, which an armed client refuses
    //! structurally before any artifact crypto. `select_authoritative_release` picks one
    //! candidate with no fallback to an older release, so that is a fleet wedge, not a
    //! delay — and until these tests existed nothing in the tree would have failed.

    use super::*;

    /// A cut signs with THIS machine's key and verifies published bytes under the
    /// key that actually signed them. Those are the same value on an ordinary cut,
    /// which is why one field did both jobs — until a machine recovered another
    /// machine's published release and `archive` tried to verify a manifest signed
    /// with Ka under Kb. The release went live, unmirrored, with its lease held by a
    /// dead publisher and no supported command able to free it.
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
            "archive/verify must use the key that actually signed the artifacts"
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
            dist: PathBuf::from("/nonexistent/repo/dist"),
            journal_path: PathBuf::from("/nonexistent/repo/dist/cut-state.toml"),
            slug: "owner/repo".to_string(),
            version: "0.5.0".to_string(),
            tag: "v0.5.0".to_string(),
            build: 500,
            commit: "a".repeat(40),
            min_build: None,
            arm64_only: false,
            manifest_signed: true,
            signature_required: true,
            signature_pubkey: None,
            verify_pubkey: None,
            signature_machine_id: machine_id.map(str::to_string),
            attribution: None,
            roster: None,
            release_id: None,
            draft_create_issued: false,
            upload_intents: Vec::new(),
            mirror_slug: Some("channel/repo".to_string()),
            mirror_release_id: None,
            mirror_create_issued: false,
            mirror_upload_intents: Vec::new(),
            kind: CutKind::Real,
            no_paint_smoke: false,
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

    /// AN ATTRIBUTED CUT attaches the roster to everything that carries assets, and an
    /// UNATTRIBUTED one — every cut this tree makes — attaches it to nothing.
    ///
    /// Kills four mutations that were all silent: `attaches_roster()` returning false,
    /// `roster_asset_paths()` returning empty, dropping the roster from the upload set,
    /// and dropping it from the draft byte-proof set.
    #[test]
    fn an_attributed_cut_carries_its_roster_through_every_asset_set() {
        let rostered = ctx(Some("m3"));
        assert!(rostered.attaches_roster());
        assert_eq!(
            names(&rostered.roster_asset_paths()),
            vec![
                roster::ROSTER_ASSET.to_string(),
                roster::ROSTER_SIG_ASSET.to_string()
            ]
        );
        for (label, set) in [
            ("upload", rostered.upload_asset_paths()),
            ("draft byte-proof", rostered.proof_asset_paths()),
            ("mirror", rostered.mirror_asset_paths()),
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

        // THE SHIPPED PATH. An unattributed cut's sets are exactly what they were
        // before the roster existed — the fleet-safety requirement, not a nicety.
        let plain = ctx(None);
        assert!(!plain.attaches_roster());
        assert!(plain.roster_asset_paths().is_empty());
        for set in [
            plain.upload_asset_paths(),
            plain.proof_asset_paths(),
            plain.mirror_asset_paths(),
        ] {
            let set = names(&set);
            assert!(
                !set.iter().any(|n| n.starts_with("aterm-machines")),
                "an unattributed cut must publish no roster: {set:?}"
            );
        }
    }

    /// The verdict's three attribution fields reach the cut TOGETHER, or the release
    /// is malformed in a way only the fleet would notice.
    ///
    /// Kills the mutations "carry the id but not the document" (the cut then stages no
    /// roster assets while the manifest claims a machine) and "carry the document but
    /// no attribution" (assets nobody is authorized by, and an unstamped manifest).
    #[test]
    fn the_verdict_hands_the_cut_all_three_attribution_fields_or_none() {
        let document = machines::RosterDocument {
            bytes: b"schema = 1\n".to_vec(),
            signature: vec![0u8; 64],
        };
        let armed = SigningVerdict {
            policy: SignaturePolicy {
                required: true,
                pubkey: Some("k".to_string()),
            },
            attribution: Some(roster::Attribution {
                machine_id: "m3".to_string(),
                pubkey_b64: "k".to_string(),
                roster_seq: 4,
            }),
            roster: Some(document.clone()),
        }
        .cut_attribution();
        assert_eq!(armed.machine_id.as_deref(), Some("m3"));
        assert_eq!(armed.attribution.map(|who| who.roster_seq), Some(4));
        assert_eq!(armed.roster, Some(document));

        let unarmed = SigningVerdict {
            policy: SignaturePolicy {
                required: false,
                pubkey: None,
            },
            attribution: None,
            roster: None,
        }
        .cut_attribution();
        assert_eq!(unarmed.machine_id, None);
        assert!(unarmed.attribution.is_none());
        assert!(unarmed.roster.is_none());
    }

    /// THE PIN EXPECTATION FOLLOWS THE COMMITTED HEAD, from whichever step asks.
    ///
    /// `aterm-gui/build.rs` embeds `__DATA,__aterm_upin` from the keyset HEAD, so that
    /// is what a shipped binary can prove. `step_build` sets the buildplan's expectation
    /// and writes the fingerprint into the provenance; `step_selfcheck` then checks the
    /// binary's `--diagnose` line and the provenance against it. Those two derived it
    /// differently — the build from the head, the self-check from the SIGNING key — and
    /// the two agree only while signer == head, the exact invariant the roster relaxes.
    ///
    /// Kills the mutation "derive from `signature_pubkey`": the assertion below then
    /// reports the signing key's fingerprint for a tree pinned to another key, which is
    /// the mismatch a rostered non-head machine would have hit after burning a ledger
    /// number and an hour of build.
    #[test]
    fn the_pin_expectation_follows_the_committed_head_from_every_step_that_asks() {
        let head = aterm_update_core::pins::update_channel_signing_pubkey();
        assert!(
            !head.is_empty(),
            "precondition: this tree pins a channel head"
        );
        // A second, obviously-synthetic key (base64 of thirty-two 0x42 bytes) — it
        // only needs to differ from the head; borrowing a real committed constant
        // here would couple this test to anchors it has no business reading.
        let other = "QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkI=";
        assert_ne!(other, head, "precondition: a second, different key");

        let mut signing_elsewhere = ctx(None);
        signing_elsewhere.signature_pubkey = Some(other.to_string());
        let mut signing_as_head = ctx(None);
        signing_as_head.signature_pubkey = Some(head.to_string());
        assert_eq!(
            signing_elsewhere.expected_embedded_pin().unwrap(),
            signing_as_head.expected_embedded_pin().unwrap(),
            "the binary embeds the COMMITTED anchor, so the cutting machine cannot move \
             what the build and the self-check expect of it"
        );
        // ...and it really is the head's fingerprint, not merely a stable one.
        assert_eq!(
            signing_elsewhere.expected_embedded_pin().unwrap(),
            expected_embedded_update_pin(Some(head), None).unwrap()
        );
        assert_ne!(
            signing_elsewhere.expected_embedded_pin().unwrap(),
            expected_embedded_update_pin(None, Some(other)).unwrap(),
            "deriving from the signer is the trap; the two must be distinguishable"
        );
    }

    /// The two READINGS the ratchet stands on: what generation the CHANNEL is at, and
    /// what generation THIS CUT carries. Both are derived from manifest bytes, and both
    /// return `None` only when there genuinely is no roster in play.
    ///
    /// Kills the mutations "always report `None` for the channel head" and "always
    /// report `None` for the cut" — either one silently disables the whole ratchet,
    /// because `roster_floor_covered` reads `None` as "no roster in play" and passes.
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
            "an unrostered head imposes no floor — the shipped state"
        );
        assert_eq!(
            published_roster_seq(Some(&head(&attributed))).unwrap(),
            Some(7)
        );

        // THE CUT's side. Before `build` the authority is the gate's attribution;
        // after it, the staged manifest — the bytes that will actually ship.
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
        // A rostered cut that can answer NEITHER is an error, never a silent `None`:
        // `None` reads as "no roster in play" and would pass the ratchet.
        let mut lost = ctx(Some("m3"));
        lost.dist = dir.join("gone");
        assert!(cut_roster_seq(&lost).is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    /// The DUTY is the same function of the same fact at all three entry points.
    ///
    /// `build` is the only step that assembles, stamps and signs a manifest, so it is
    /// the only fact that can decide whether an entry has a roster question to answer.
    /// The bug this closes was `resume_cut` guarding on it, `revalidate_ctx_signature_policy`
    /// not, and `run_recover_lost` stating a third rule.
    #[test]
    fn the_duty_is_decided_by_one_fact_and_shared_by_every_entry() {
        assert_eq!(roster_duty(false), RosterDuty::Sign);
        assert_eq!(roster_duty(true), RosterDuty::Finish);
    }

    /// A recovery reconstructs the roster pair from what the PUBLISHED MANIFEST says
    /// about itself, so the set it rebuilds is the set `mirror_asset_paths` will demand.
    ///
    /// Kills the mutation "reconstruct nothing": the two sets then disagree, and
    /// `step_mirror` dies on a file recovery never wrote — with advice ("recover the
    /// cut") naming the command that was already running.
    #[test]
    fn recovery_rebuilds_exactly_the_roster_assets_the_mirror_will_demand() {
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
        // An UNATTRIBUTED published release reconstructs no roster, and demands none.
        assert!(recovered_roster_asset_names(&manifest).is_empty());
        assert!(
            ctx(manifest.machine_id.as_deref())
                .roster_asset_paths()
                .is_empty()
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
        let demanded = names(&ctx(manifest.machine_id.as_deref()).mirror_asset_paths());
        for name in &rebuilt {
            assert!(demanded.contains(name), "{name} rebuilt but not demanded");
        }
        for name in demanded.iter().filter(|n| n.starts_with("aterm-machines")) {
            assert!(rebuilt.contains(name), "{name} demanded but never rebuilt");
        }
        assert_eq!(rebuilt.len(), 2, "{rebuilt:?}");
    }

    /// A RECOVERY MUST NOT RETIRE A NEWER ROSTER.
    ///
    /// `dist/aterm-machines.toml` is the machine's AUTHORIZING roster — the file
    /// `atpkg-keys` writes and `ReleaseCredentials::resolve` adopts — and `dist/` is
    /// gitignored, so it is the only copy. Recovery used to overwrite it in place with
    /// whatever generation the recovered release carried, which silently destroys a
    /// newer master-signed document and any revocation inside it, recreatable only from
    /// the paper master. Nothing downstream noticed: `roster_floor_covered` compares the
    /// carried generation against the published head, and after the downgrade both
    /// agree.
    ///
    /// The fixture is the real generation-2 pair published on the channel, so the guard
    /// is exercised against bytes that genuinely verify under the pinned paper master.
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

        // An OLDER release: refused, naming the file it just protected.
        let err = refuse_roster_downgrade(&dir, 1).unwrap_err();
        assert!(err.0.contains("NEWER"), "{}", err.0);
        assert!(err.0.contains(roster::ROSTER_ASSET), "{}", err.0);

        // Same generation, or a newer one: nothing is being lost.
        assert!(refuse_roster_downgrade(&dir, 2).is_ok());
        assert!(refuse_roster_downgrade(&dir, 3).is_ok());

        // A pair that does NOT verify under the pinned master is not an authorizing
        // document, and must never be able to veto un-wedging the release pipeline.
        std::fs::write(dir.join(roster::ROSTER_SIG_ASSET), [0u8; 64]).unwrap();
        assert!(refuse_roster_downgrade(&dir, 1).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A release that names no machine predates the roster tier, so the committed
    /// channel keyset the policy already carries stays the authority. This path must
    /// not touch the network — it returns before any asset download.
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
        .expect("a pre-roster manifest resolves without any download");
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
