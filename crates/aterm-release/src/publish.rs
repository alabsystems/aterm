// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Release publication pipeline, GitHub draft/release management, and asset upload verification.
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aterm_digest::Sha256;
use aterm_uds::spawnfd::command;
use serde::{Deserialize, Serialize};

use aterm_update_core::Manifest;
use aterm_update_core::roster;
use aterm_update_core::tag::TagError;

use crate::ledger::{
    self, ControlRefChange, ControlRefTransaction, Error, GitCli, GitRunner, Origin, RefWrite,
    Result, RunOut, git_ok, rev_parse,
};
use crate::{
    artifact, buildplan, bundle, changelog, channel, dmg, gates, machines, manifest_out, sign,
    verify,
};

#[path = "publish/draft.rs"]
pub(crate) mod draft;

/// Every `cut` flag, parsed by `cli.rs`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CutOptions {
    pub release_credentials: Option<PathBuf>,
    pub dry_run: bool,
    pub resume: bool,
    pub min_build: Option<u64>,
    pub linux_artifacts: Option<PathBuf>,
    pub linux_targets: Vec<String>,
    pub linux_workers: Vec<String>,
    pub mac_only: bool,
    /// `--windows-artifacts DIRECTORY`: the Windows handoff directory
    /// ([`buildplan::windows::Handoff`]); optional, and independent of the Linux declaration.
    pub windows_artifacts: Option<PathBuf>,
    /// `--windows-sha256 HEX`: the Windows zip's SHA-256 as `release-build` printed it on the
    /// Windows host, lowercase; no carried pair is admitted without it
    /// ([`buildplan::windows::Handoff::sha256`]).
    pub windows_sha256: Option<String>,
    /// `--sign`: sign, notarize and package a byte copy of the release's verified bundle under
    /// its reserved number, privately, and keep the record the claim adopts ([`run_cut_sign`]).
    pub sign: bool,
    /// Private packaging half only: no product executions and no adoptable record.
    pub sign_prepare: bool,
}

/// Which cut flavor is running — decided once, checked per step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutKind {
    Real,
    DryRun,
    /// Private uploads under the release's reservation, before its ledger claim.
    Draft,
}

/// How an operator runs a cut.
pub const CUT_COMMAND: &str = crate::release::RELEASE_COMMAND;

/// How an operator runs the cutter's other verbs.
pub const SHIP_COMMAND: &str = "targo --unverified ship";

/// The column every value starts in.
pub const VALUE_COL: usize = 13;

/// The longest label the grid carries.
pub const LABEL_MAX: usize = VALUE_COL - 3;

fn width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| {
            let v = v.trim();
            (!v.starts_with('+'))
                .then(|| v.parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(100)
        .clamp(60, 120)
}

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
        let hang_chars = lead + marker.chars().count();
        let hang = " ".repeat(hang_chars);
        let mut line = String::from(&seg[..lead]);
        line.push_str(marker);
        let mut line_chars = hang_chars;
        let mut filled = false;
        let mut rest = &body[marker.len()..];
        while !rest.is_empty() {
            let gap = rest.len() - rest.trim_start_matches(' ').len();
            rest = &rest[gap..];
            if rest.is_empty() {
                break;
            }
            let end = rest.find(' ').unwrap_or(rest.len());
            let (word, tail) = rest.split_at(end);
            rest = tail;
            let word_chars = word.chars().count();
            if filled && line_chars + gap + word_chars > budget {
                out.push(std::mem::replace(&mut line, hang.clone()));
                line_chars = hang_chars;
                filled = false;
            }
            if filled {
                for _ in 0..gap {
                    line.push(' ');
                }
                line_chars += gap;
            }
            line.push_str(word);
            line_chars += word_chars;
            filled = true;
        }
        out.push(line);
    }
    if msg.ends_with(' ')
        && !msg.trim().is_empty()
        && let Some(last) = out.last_mut()
        && !last.is_empty()
    {
        last.push(' ');
    }
    out
}

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

/// Exactly what [`step`] prints, with no trailing newline, so a prompt written to `/dev/tty` sits in the transcript's own grid.
pub fn grid_block(label: &str, msg: &str) -> String {
    grid_block_at(width(), label, msg)
}

/// [`grid_block`] at an explicit width.
pub fn grid_block_at(width: usize, label: &str, msg: &str) -> String {
    if msg.trim().is_empty() {
        return String::new();
    }
    let gutter = " ".repeat(VALUE_COL);
    let mut out = String::new();
    let mut label_placed = label.chars().count() > LABEL_MAX;
    if label_placed {
        for row in wrapped(label, width.saturating_add(VALUE_COL - 2)) {
            if !row.trim().is_empty() {
                out.push_str("  ");
                out.push_str(&row);
                out.push('\n');
            }
        }
    }
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
            use std::fmt::Write as _;
            let _ = write!(out, "  {label:<LABEL_MAX$} ");
            label_placed = true;
        }
        out.push_str(row);
    }
    out
}

/// One transcript line (or block).
pub fn step(label: &str, msg: &str) {
    println!("{}", grid_block(label, msg));
}

#[cfg(test)]
#[path = "publish_grid_label_tests.rs"]
mod grid_label_tests;

/// "4m12s" / "38s" — whole-cut timing for the DONE line.
pub fn fmt_elapsed(start: Instant) -> String {
    let s = start.elapsed().as_secs();
    if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// Read the source tree's `[workspace.package]` product version.
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

/// The version a cut publishes.
pub fn release_version_from_workspace(workspace: &str) -> Result<String> {
    product_version(workspace)?;
    Ok(workspace.to_string())
}

fn product_version(version: &str) -> Result<aterm_types::release_version::ReleaseVersion> {
    aterm_types::release_version::ReleaseVersion::parse(version).ok_or_else(|| Error::new(format!(
        "version {version:?} is not a historical MAJOR.MINOR.0 or calendar 1.YYYYMMDD.N product release"
    )))
}

/// Select a new cut identity from the committed product version at a fixed UTC time.
/// Legacy sources keep their existing minor policy until a source claim adopts calendar form.
pub fn allocate_release_after(release: &str, selected_at: u64) -> Result<String> {
    product_version(release)?
        .next_at(selected_at)
        .map(|v| v.to_string())
        .map_err(|e| Error::new(e.to_string()))
}

/// "owner/repo" from `[workspace.package] repository`.
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
            let tail = tail.trim_end_matches('/');
            let slug = tail.strip_suffix(".git").unwrap_or(tail);
            if slug.split('/').count() == 2 {
                return Some(slug.to_string());
            }
        }
    }
    None
}

fn workspace_repo_slug(repo: &Path) -> Result<String> {
    let path = repo.join("Cargo.toml");
    let cargo_text = gates::read_bounded_string(&path, 16 << 20)
        .map_err(|error| Error::new(format!("read {}: {error}", path.display())))?;
    repo_slug(&cargo_text).ok_or_else(|| {
        Error::new(format!(
            "{} [workspace.package] repository is not an exact GitHub OWNER/REPO URL",
            path.display()
        ))
    })
}

/// The release channel (`OWNER/REPO`) of `cargo_toml`.
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
    let cargo_text = gates::read_bounded_string(&path, 16 << 20)
        .map_err(|error| Error::new(format!("read {}: {error}", path.display())))?;
    channel_slug(&cargo_text)
}

/// Parse the GitHub repository addressed by an `origin` URL.
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
    let tail = tail.trim_end_matches('/');
    let slug = tail.strip_suffix(".git").unwrap_or(tail);
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

/// Bind the git remote used for lease/tag/CAS operations to the Cargo.toml `[workspace.package] repository`.
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

/// One clock reading for the whole cut.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The release-org token's registry entry: the vault lands it on every cutting Mac.
pub(crate) const CHANNEL_TOKEN: &str = aterm_secrets::entry_class::RELEASE_GH_TOKEN;

/// The release-org token on disk: where the vault lands [`CHANNEL_TOKEN`].
pub(crate) fn channel_token_path() -> Option<PathBuf> {
    std::env::var_os("HOME").and_then(|home| {
        aterm_secrets::materialize::target_path(Path::new(&home), CHANNEL_TOKEN).ok()
    })
}

/// [`CHANNEL_TOKEN`]'s registry pointer (`~/…`), as `aterm secrets ls --describe` names it.
pub(crate) fn channel_token_pointer() -> String {
    aterm_secrets::registry::pointer(CHANNEL_TOKEN).unwrap_or_else(|| CHANNEL_TOKEN.to_owned())
}

/// What a gate says when this Mac holds no release-org token: the pointer, and the one way
/// to get it.
pub(crate) fn no_channel_token() -> String {
    format!(
        "this Mac holds no release-org token at {} (`aterm secrets need gh:release` asks the \
         lab's Macs for it)",
        channel_token_pointer()
    )
}

/// Dedicated cooperative lock for every real release cut.
pub use agh::git::RELEASE_LEASE_REF;

/// Per-invocation fencing token.
pub use agh::git::PUBLISHER_FENCE_REF;

/// Mandatory acknowledgement for the one recovery operation whose safety has an external, operator-established precondition.
pub const RECOVERY_STOPPED_PROCESS_FLAG: &str = "--old-publisher-stopped";

/// The operator's assertion that no draft was ever posted for this tag.
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

/// Authoritative remote fence state.
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
    AlreadySuperseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceRelease {
    Released,
    AlreadyAbsent,
    AlreadySuperseded,
}

fn valid_lease_owner(owner: &str) -> bool {
    matches!(owner.len(), 40 | 64) && owner.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Pure `AcquireLease` decision seam used by production and Tier-1 bindings.
pub fn acquire_lease_action(
    observed: Option<&str>,
    expected_owner: &str,
) -> Result<LeaseAcquireAction> {
    if !valid_lease_owner(expected_owner) {
        return Err(Error::new(format!(
            "release lease owner {expected_owner:?} is not a full git object id"
        )));
    }
    match observed {
        None => Ok(LeaseAcquireAction::Create),
        Some(owner) if owner.eq_ignore_ascii_case(expected_owner) => {
            Ok(LeaseAcquireAction::AlreadyOwned)
        }
        Some(owner) => Err(Error::new(format!(
            "release lease {RELEASE_LEASE_REF} is owned by {}, not {}; \
             refusing to steal or force-update it",
            owner.to_ascii_lowercase(),
            expected_owner.to_ascii_lowercase(),
        ))),
    }
}

pub(crate) fn native_origin(repo: &Path) -> Result<crate::github_remote::Remote> {
    let _timing = crate::cut_timing::Timer::new("native publication origin");
    let expected = workspace_repo_slug(repo)?;
    let origin = crate::github_remote::Remote::origin(repo).map_err(Error::new)?;
    if !origin.repository().eq_full_name(&expected) {
        return Err(Error::new(
            "native publication origin differs from Cargo.toml workspace repository; refusing ref operations",
        ));
    }
    Ok(origin)
}

fn write_control_refs(
    origin: &dyn Origin,
    owner: &str,
    changes: Vec<ControlRefChange>,
) -> Result<RefWrite> {
    let transaction = ControlRefTransaction::new(owner, changes)
        .map_err(|error| Error::new(error.why.clone()))?;
    write_control_transaction(origin, transaction, &mut |line| eprintln!("{line}"))
}

fn write_control_transaction(
    origin: &dyn Origin,
    transaction: ControlRefTransaction,
    log: &mut dyn FnMut(&str),
) -> Result<RefWrite> {
    const ATTEMPTS: usize = 3;
    for attempt in 1..=ATTEMPTS {
        let written = origin
            .control_transaction(transaction.clone())
            .map_err(Error::new)?;
        let RefWrite::Transient(cause) = written else {
            return Ok(written);
        };
        // Read both canonical refs, including the invariant on the sibling
        // of a single-ref change. Every repeated write still carries its CAS.
        let reread = |reference| {
            origin.get(reference).map_err(|why| {
                Error::new(format!(
                    "control transaction re-read of {reference} failed after {cause}: {why}"
                ))
            })
        };
        let lease = reread(RELEASE_LEASE_REF)?;
        let fence = reread(PUBLISHER_FENCE_REF)?;
        let unchanged = transaction.changes().iter().all(|change| {
            let observed = if change.reference == RELEASE_LEASE_REF {
                &lease
            } else {
                &fence
            };
            observed == &change.before
        });
        let sibling_ok = transaction.changes().len() == 2 || {
            let change = &transaction.changes()[0];
            if change.reference == RELEASE_LEASE_REF {
                fence.is_none()
            } else if change.after.is_some() {
                lease.as_deref() == Some(transaction.owner_commit())
            } else {
                true
            }
        };
        if !unchanged || !sibling_ok {
            log(&format!(
                "control transaction precondition changed after: {cause}"
            ));
            return Ok(RefWrite::Conflict);
        }
        if attempt == ATTEMPTS {
            return Err(Error::new(format!(
                "control transaction exhausted {ATTEMPTS} attempts with expected refs unchanged: {cause}"
            )));
        }
        log(&format!(
            "control transaction attempt {attempt}/{ATTEMPTS}: {cause}; re-read confirmed expected refs; repeating the same expected-old transaction"
        ));
    }
    unreachable!("bounded control transaction returns on its last attempt")
}

#[cfg(test)]
#[path = "publish/control_retry_tests.rs"]
mod control_retry_tests;

fn ref_write_description(written: RefWrite) -> String {
    match written {
        RefWrite::Written => "native expected-old transaction completed".into(),
        RefWrite::Conflict => {
            "native expected-old transaction conflicted without side effects".into()
        }
        RefWrite::Transient(cause) => cause,
    }
}

/// Read the exact lightweight lock through the authenticated native origin.
pub fn release_lease_owner(origin: &dyn Origin) -> Result<Option<String>> {
    checked_lease_owner(origin.get(RELEASE_LEASE_REF)?)
}

fn checked_lease_owner(owner: Option<String>) -> Result<Option<String>> {
    owner
        .map(|owner| {
            if !valid_lease_owner(&owner) {
                return Err(Error::new("release lease is not one raw object ID"));
            }
            Ok(owner.to_ascii_lowercase())
        })
        .transpose()
}

/// Read the unique annotated publisher fence and its peeled claim.
pub fn publisher_fence(origin: &dyn Origin) -> Result<Option<PublisherFence>> {
    let Some(observed) = remote_annotated_ref(origin, PUBLISHER_FENCE_REF, "publisher fence")?
    else {
        return Ok(None);
    };
    Ok(Some(PublisherFence {
        token: observed.token,
        owner: observed.commit,
    }))
}

fn publisher_fence_for_guard(
    origin: &dyn Origin,
    guard: &PublisherFenceGuard,
) -> Result<Option<PublisherFence>> {
    fence_from_observed_token(origin, guard, origin.get(PUBLISHER_FENCE_REF)?)
}

fn fence_from_observed_token(
    origin: &dyn Origin,
    guard: &PublisherFenceGuard,
    token: Option<String>,
) -> Result<Option<PublisherFence>> {
    let Some(token) = token else {
        return Ok(None);
    };
    if token.eq_ignore_ascii_case(guard.token()) {
        // The private guard was minted only after importing and validating this
        // exact annotation. Its content-addressed owner cannot change while the
        // freshly observed token stays identical; no history import is needed.
        return Ok(Some(PublisherFence {
            token: guard.token.clone(),
            owner: guard.owner.clone(),
        }));
    }
    let observed = resolve_annotated_token(origin, token, "publisher fence")?;
    Ok(Some(PublisherFence {
        token: observed.token,
        owner: observed.commit,
    }))
}

/// Version of the `fence-*` identity block in the fence tag body. Format 1 recorded the
/// publisher (pid, host, boot, exe, version); 2 added its process group (`fence-pgid`);
/// 3 names the boot by the kernel's boot id (`fence-boot`, [`boot_text`]) where 1 and 2
/// stamped it (`kern.boottime:<secs>`, `linux-boot-id:<id>`) for a text comparison, so a
/// reader of 2 or older refuses a fence of 3 as newer than itself rather than read a boot
/// id as another boot.
pub const FENCE_IDENTITY_FORMAT: u32 = 3;

/// The last format whose `fence-boot` was a stamp rather than a boot id.
const LAST_STAMPED_BOOT_FORMAT: u32 = 2;

/// A boot id as `fence-boot` writes it: astream's text of it ([`astream_sys::boot_id_text`],
/// the UUID's 32 hex digits hyphenated 8-4-4-4-12) in the case this host's kernel spells it,
/// so the line reads as `sysctl kern.bootsessionuuid` (or Linux's `boot_id`) prints it. A
/// reader compares the bytes ([`fence_liveness`]), never this text.
pub fn boot_text(id: [u8; 16]) -> String {
    astream_sys::boot_id_text(id, astream_sys::KERNEL_SPELLS_BOOT_ID_LOWER)
}

static FENCE_SELF_VERSION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Record what this invocation publishes.
pub fn set_publisher_fence_version(version: &str) {
    if let Ok(mut slot) = FENCE_SELF_VERSION.lock() {
        let v = version.trim();
        *slot = Some(v.strip_prefix('v').unwrap_or(v).to_string());
    }
}

fn publisher_fence_self_version() -> Option<String> {
    FENCE_SELF_VERSION.lock().ok().and_then(|slot| slot.clone())
}

/// Keep each field on one line. Display-width truncation can remove an executable's
/// basename and make a live publisher look like a reused pid.
fn fence_field(value: &str) -> std::borrow::Cow<'_, str> {
    if !value.chars().any(char::is_control) {
        let trimmed = value.trim();
        return if trimmed.is_empty() {
            std::borrow::Cow::Borrowed("unknown")
        } else {
            std::borrow::Cow::Borrowed(trimmed)
        };
    }
    let out: String = value
        .chars()
        .map(|c| if c.is_control() { '_' } else { c })
        .collect();
    let trimmed = out.trim();
    if trimmed.is_empty() {
        std::borrow::Cow::Borrowed("unknown")
    } else if trimmed.len() == out.len() {
        std::borrow::Cow::Owned(out)
    } else {
        std::borrow::Cow::Owned(trimmed.to_string())
    }
}

pub(crate) fn fence_identity_block(probe: &dyn PublisherProbe) -> String {
    use std::fmt::Write as _;
    let mut out = format!(
        "fence-format: {FENCE_IDENTITY_FORMAT}\nfence-pid: {}\n",
        std::process::id()
    );
    if let Some(pgid) = probe.pgid() {
        let _ = writeln!(out, "fence-pgid: {pgid}");
    }
    for (key, value) in [
        ("host", probe.host()),
        ("boot", probe.boot().map(boot_text)),
        ("exe", probe.exe()),
        ("version", publisher_fence_self_version()),
    ] {
        if let Some(value) = value {
            let _ = writeln!(out, "fence-{key}: {}", fence_field(&value));
        }
    }
    if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
        let _ = writeln!(out, "fence-started: {}", now.as_secs());
    }
    out
}

/// Who wrote a fence, as read back out of its annotated-tag body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FenceIdentity {
    pub format: Option<u32>,
    pub pid: Option<u32>,
    pub pgid: Option<u32>,
    pub host: Option<String>,
    pub boot: Option<String>,
    pub exe: Option<String>,
    pub version: Option<String>,
    pub started_unix: Option<u64>,
    pub unreadable: Option<String>,
}

impl FenceIdentity {
    fn unreadable(reason: impl Into<String>) -> Self {
        FenceIdentity {
            unreadable: Some(reason.into()),
            ..FenceIdentity::default()
        }
    }

    /// Parse the `fence-*` lines out of a `git cat-file tag` body.
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
            let uint = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
            match key.trim() {
                "format" if uint(value) => identity.format = value.parse().ok(),
                "pid" if uint(value) => identity.pid = value.parse().ok(),
                "pgid" if uint(value) => identity.pgid = value.parse().ok(),
                "host" => identity.host = Some(value.to_string()),
                "boot" => identity.boot = Some(value.to_string()),
                "exe" => identity.exe = Some(value.to_string()),
                "version" => {
                    identity.version = Some(value.strip_prefix('v').unwrap_or(value).to_string());
                }
                "started" if uint(value) => identity.started_unix = value.parse().ok(),
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
    Running { exe: Option<String> },
    NotRunning,
    Unknown(String),
}

/// What the process table says about one recorded process group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupState {
    Empty,
    Members(Vec<(u32, String)>),
    Unknown(String),
}

/// The machine facts liveness needs.
pub trait PublisherProbe {
    fn host(&self) -> Option<String>;
    /// This boot's id, its 16 bytes (`astream_sys::boot_session`).
    fn boot(&self) -> Option<[u8; 16]>;
    fn exe(&self) -> Option<String>;
    fn pid_state(&self, pid: u32) -> PidState;
    fn pgid(&self) -> Option<u32> {
        None
    }
    fn group_members(&self, pgid: u32) -> GroupState {
        let _ = pgid;
        GroupState::Unknown("this probe does not report process groups".to_string())
    }
}

/// The real machine.
pub struct LocalProbe;

impl LocalProbe {
    /// This process's group: `getpgrp`, which cannot fail.
    #[cfg(unix)]
    fn own_pgid() -> Option<u32> {
        unsafe extern "C" {
            fn getpgrp() -> i32;
        }
        // SAFETY: `getpgrp` takes nothing, touches no memory of ours and always succeeds.
        u32::try_from(unsafe { getpgrp() }).ok()
    }

    /// See the unix twin: no group is read here.
    #[cfg(not(unix))]
    fn own_pgid() -> Option<u32> {
        None
    }

    /// The members of `pgid` in a listing of every process read in this process
    /// ([`crate::interrupt::listing`]), each named by the kernel's short name for it. No program
    /// runs and no process's arguments are read: a `ps` that reads every process's hung by the
    /// thousand in a program-check wedge (`docs/DESIGN-leftover-processes-2026-10-16.md` M29).
    fn members_of(pgid: u32) -> GroupState {
        let Some(procs) = crate::interrupt::listing() else {
            return GroupState::Unknown("the process table could not be read".to_string());
        };
        Self::group_of(procs, pgid, std::process::id())
    }

    /// [`Self::members_of`] over `procs`, a listing that must show `me`.
    fn group_of(procs: Vec<crate::interrupt::Proc>, pgid: u32, me: u32) -> GroupState {
        let own = |pid: i32| u32::try_from(pid).ok();
        if !procs.iter().any(|p| own(p.pid) == Some(me)) {
            return GroupState::Unknown("the process table did not list this process".to_string());
        }
        let members: Vec<(u32, String)> = procs
            .into_iter()
            .filter(|p| u32::try_from(p.pgid).ok() == Some(pgid))
            .filter_map(|p| Some((own(p.pid)?, p.name)))
            .collect();
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
        HOST.get_or_init(aterm_uds::process::kernel_host_name)
            .clone()
    }

    fn boot(&self) -> Option<[u8; 16]> {
        astream_sys::boot_session()
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
        if Path::new("/proc/self/stat").exists() {
            let dir = PathBuf::from(format!("/proc/{pid}"));
            if !dir.exists() {
                return PidState::NotRunning;
            }
            let exe = fs::read_link(dir.join("exe"))
                .ok()
                .map(|path| path.display().to_string())
                .or_else(|| {
                    aterm_uds::process::read_proc_text(&dir.join("comm"))
                        .ok()
                        .map(|name| name.trim().to_string())
                });
            return PidState::Running { exe };
        }
        // macOS's process table, read in this process: a zombie reads as `ps` prints it, and a
        // live process as `ps -o comm=` names it, its first argument word
        // (`kproc::ps_comm`), the path it was started by, as `current_exe` records the
        // publisher's, a symlink's own name included. Its words are read only once its one
        // thread has run (`thread_ran`, M29 of `docs/DESIGN-leftover-processes-2026-10-16.md`);
        // before that it is named by the image it runs (`proc_pidpath`, or the kernel's short
        // name where that does not read).
        #[cfg(target_os = "macos")]
        {
            match aterm_sysprobe::kproc::record_read(pid) {
                Ok(None) => PidState::NotRunning,
                Err(error) => PidState::Unknown(format!(
                    "the kernel's record of pid {pid} did not read: {error}"
                )),
                Ok(Some(r)) if r.zombie => PidState::Running {
                    exe: Some(aterm_sysprobe::kproc::DEFUNCT.to_string()),
                },
                Ok(Some(r)) => {
                    let ran = i32::try_from(pid)
                        .ok()
                        .and_then(aterm_uds::process::thread_ran);
                    let exe = if ran == Some(true) {
                        aterm_sysprobe::kproc::ps_comm(&r, aterm_sysprobe::kproc::Locale::Utf8)
                    } else {
                        aterm_sysprobe::process_path(pid)
                            .map_or(r.comm, |path| path.display().to_string())
                    };
                    PidState::Running { exe: Some(exe) }
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            PidState::Unknown(format!(
                "pid {pid}: this platform's process table is not read"
            ))
        }
    }
}

fn executables_agree(recorded: &str, observed: &str) -> bool {
    fn base(path: &str) -> &str {
        let trimmed = path.trim();
        trimmed
            .strip_suffix(" (deleted)")
            .unwrap_or(trimmed)
            .rsplit('/')
            .next()
            .unwrap_or(path)
            .trim()
    }
    let recorded = base(recorded);
    let observed = base(observed);
    if recorded.is_empty() || observed.is_empty() || recorded == observed {
        return true;
    }
    let (short, long) = if recorded.len() <= observed.len() {
        (recorded, observed)
    } else {
        (observed, recorded)
    };
    short.len() >= 8 && long.starts_with(short)
}

/// Is the publisher that wrote this fence alive, dead, or unprovable?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceLiveness {
    Alive(String),
    Dead(String),
    Unprovable(String),
}

impl FenceLiveness {
    pub const fn label(&self) -> &'static str {
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
    let Some(recorded_boot) = astream_sys::parse_boot_id(boot.as_bytes()) else {
        let pre_astream = format <= LAST_STAMPED_BOOT_FORMAT
            && ["kern.boottime:", "linux-boot-id:"]
                .iter()
                .any(|stamp| boot.starts_with(stamp));
        return FenceLiveness::Unprovable(if pre_astream {
            format!(
                "the fence names its boot session in a pre-astream form ({boot}), which an older \
                 aterm-release wrote; this binary compares boot ids only, so it cannot tell \
                 whether the recorded pid belongs to this boot — it is not proof of death"
            )
        } else {
            format!(
                "the fence's boot session ({boot}) is not a boot id, so this cannot tell whether \
                 the recorded pid belongs to this boot — it is not proof of death"
            )
        });
    };
    if recorded_boot != local_boot {
        return FenceLiveness::Unprovable(format!(
            "the fence was written in boot session {boot}; this machine is now in {}. \
             The recorded pid cannot be that publisher any more, but a hostname is not a unique \
             machine identity, so this cannot separate \"this machine rebooted\" from \"another \
             machine with the same name wrote it\" — it is not proof of death",
            boot_text(local_boot)
        ));
    }
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
    let Some(pgid) = identity.pgid else {
        return FenceLiveness::Unprovable(format!(
            "{publisher_gone} — but the fence records no process group (fence-format {format}, \
             written before groups were recorded), and every GitHub request that publisher \
             made ran in a spawned child that a killed parent does not stop. Whether one \
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

/// The self-diagnosing refusal.
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
                "\n  NOTE: the fence records no version ({}); v{version} is THIS invocation's \
                 version — confirm it against the stuck machine's dist/cut-state.toml before \
                 running the command",
                match (&identity.unreadable, identity.format) {
                    (Some(_), _) => "its body could not be read",
                    (None, None) => "it predates fence-format 1, the first to record one",
                    (None, Some(_)) => "the publisher that wrote it had none to record",
                }
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

/// Read the identity block out of the fence's annotated-tag object.
pub fn publisher_fence_identity(origin: &dyn Origin, token: &str) -> FenceIdentity {
    if !valid_lease_owner(token) {
        return FenceIdentity::unreadable(format!("malformed fence token {token:?}"));
    }
    if let Err(error) = origin.import(token) {
        return FenceIdentity::unreadable(format!("cannot import exact fence {token}: {error}"));
    }
    match origin.tag_object(token) {
        Ok(body) => FenceIdentity::parse(&String::from_utf8_lossy(&body)),
        Err(error) => {
            FenceIdentity::unreadable(format!("cannot read exact fence {token}: {error}"))
        }
    }
}

/// Outcome of a resume's liveness probe against an active fence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceReclaim {
    NoFence,
    Reclaimed { token: String, detail: String },
    Kept { refusal: String },
}

/// `--resume`'s automatic reclaim.
pub fn reclaim_dead_publisher_fence(
    origin: &dyn Origin,
    expected_owner: &str,
    probe: &dyn PublisherProbe,
) -> Result<FenceReclaim> {
    let Some(fence) = publisher_fence(origin)? else {
        return Ok(FenceReclaim::NoFence);
    };
    let identity = publisher_fence_identity(origin, &fence.token);
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
    if !fence.owner.eq_ignore_ascii_case(expected_owner) {
        return Ok(FenceReclaim::Kept {
            refusal: refusal(Some(
                "not reclaimable: the fence peels to a different claim than this resume's",
            )),
        });
    }
    if !release_lease_owner(origin)?
        .as_deref()
        .is_some_and(|o| o.eq_ignore_ascii_case(expected_owner))
    {
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
    let written = write_control_refs(
        origin,
        &fence.owner,
        vec![ControlRefChange {
            reference: PUBLISHER_FENCE_REF.into(),
            before: Some(fence.token.clone()),
            after: None,
        }],
    )?;
    match publisher_fence(origin)? {
        None => Ok(FenceReclaim::Reclaimed {
            token: fence.token.clone(),
            detail: detail.clone(),
        }),
        Some(current) if current.token != fence.token => {
            let identity = publisher_fence_identity(origin, &current.token);
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
                ref_write_description(written)
            ))),
        }),
    }
}

fn ensure_no_publisher_fence(origin: &dyn Origin) -> Result<()> {
    if let Some(fence) = publisher_fence(origin)? {
        let identity = publisher_fence_identity(origin, &fence.token);
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

/// Read-only fresh-cut preflight.
pub fn preflight_release_lease(origin: &dyn Origin) -> Result<()> {
    ensure_no_publisher_fence(origin)?;
    if let Some(owner) = release_lease_owner(origin)? {
        return Err(Error::new(format!(
            "the release lock is held by claim {owner}; nothing was claimed. On the machine \
             that cut it: `{CUT_COMMAND}` or `--abandon vX.Y.Z`; from another machine, \
             once that cut is stopped: `{}`",
            fence_recover_command(None, &owner)
        )));
    }
    Ok(())
}

/// The cut's pre-claim gates that need nothing `pub publish` makes. A private cut does not require the public publisher's lease to be idle.
pub fn pre_cut_gates(
    repo: &Path,
    version: &str,
    creds_path: &Path,
    kind: CutKind,
) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let git = GitCli::new(repo);
    if let Err(e) =
        gates::staged_bundle_liveness_gate(&repo.join("dist"), &crate::bundle::running_processes)
    {
        out.push(("staged bundle", e.to_string()));
    }
    if kind == CutKind::Real
        && let Err(e) = native_origin(repo).and_then(|origin| preflight_release_lease(&origin))
    {
        out.push(("release lease", e.to_string()));
    }
    let slug = match workspace_channel_slug(repo) {
        Ok(slug) => slug,
        Err(e) => {
            out.push(("channel", e.to_string()));
            return out;
        }
    };
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

/// Production `AcquireLease`.
pub fn acquire_release_lease(
    origin: &dyn Origin,
    expected_owner: &str,
) -> Result<ReleaseLeaseGuard> {
    ensure_no_publisher_fence(origin)?;
    let expected_owner = expected_owner.to_ascii_lowercase();
    let observed = release_lease_owner(origin)?;
    if acquire_lease_action(observed.as_deref(), &expected_owner)? == LeaseAcquireAction::Create {
        let written = write_control_refs(
            origin,
            &expected_owner,
            vec![ControlRefChange {
                reference: RELEASE_LEASE_REF.into(),
                before: None,
                after: Some(expected_owner.clone()),
            }],
        )?;
        if written == RefWrite::Conflict {
            let now = release_lease_owner(origin)?;
            if now.as_deref() != Some(expected_owner.as_str()) {
                return Err(Error::new(format!(
                    "atomic release lease create lost: {}; owner is {}",
                    "native expected-old conflict",
                    now.as_deref().unwrap_or("absent")
                )));
            }
        }
    }
    confirm_release_lease_owner(origin, &expected_owner)
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
    origin: &dyn Origin,
    expected_owner: &str,
) -> Result<ReleaseLeaseGuard> {
    let observed = release_lease_owner(origin)?;
    if !observed
        .as_deref()
        .is_some_and(|o| o.eq_ignore_ascii_case(expected_owner))
    {
        return Err(Error::new(format!(
            "release lease ownership changed: expected {}, observed {}",
            expected_owner.to_ascii_lowercase(),
            observed.as_deref().unwrap_or("absent")
        )));
    }
    Ok(ReleaseLeaseGuard {
        owner: observed.unwrap_or_else(|| expected_owner.to_ascii_lowercase()),
    })
}

/// Create a unique per-process fence.
pub fn acquire_publisher_fence(
    git: &dyn GitRunner,
    origin: &dyn Origin,
    expected_owner: &str,
) -> Result<PublisherFenceGuard> {
    let lease = confirm_release_lease_owner(origin, expected_owner)?;
    ensure_no_publisher_fence(origin)?;
    let token = new_publisher_fence_token(git, expected_owner)?;
    let written = write_control_refs(
        origin,
        expected_owner,
        vec![ControlRefChange {
            reference: PUBLISHER_FENCE_REF.into(),
            before: None,
            after: Some(token.clone()),
        }],
    )?;
    let now = publisher_fence(origin)?;
    if now.as_ref().is_some_and(|fence| {
        fence.token == token && fence.owner.eq_ignore_ascii_case(expected_owner)
    }) {
        return Ok(PublisherFenceGuard {
            owner: lease.owner,
            token,
        });
    }
    Err(Error::new(format!(
        "atomic publisher-fence create lost: {}; current token is {}",
        ref_write_description(written),
        now.as_ref().map_or("absent", |fence| fence.token.as_str())
    )))
}

/// Explicit killed-machine takeover.
pub fn rotate_publisher_fence_for_recovery(
    git: &dyn GitRunner,
    origin: &dyn Origin,
    expected_owner: &str,
) -> Result<PublisherFenceGuard> {
    let lease = confirm_release_lease_owner(origin, expected_owner)?;
    let observed = publisher_fence(origin)?;
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
    let written = write_control_refs(
        origin,
        expected_owner,
        vec![ControlRefChange {
            reference: PUBLISHER_FENCE_REF.into(),
            before: observed.as_ref().map(|fence| fence.token.clone()),
            after: Some(token.clone()),
        }],
    )?;
    let now = publisher_fence(origin)?;
    if now.as_ref().is_some_and(|fence| {
        fence.token == token && fence.owner.eq_ignore_ascii_case(expected_owner)
    }) {
        return Ok(PublisherFenceGuard {
            owner: lease.owner,
            token,
        });
    }
    Err(Error::new(format!(
        "publisher-fence recovery rotation lost its exact CAS: {}; current token is {}",
        ref_write_description(written),
        now.as_ref().map_or("absent", |fence| fence.token.as_str())
    )))
}

/// Re-prove both persistent claim ownership and the exact process token before each release mutation.
pub fn assert_publisher_session(
    origin: &dyn Origin,
    lease: &ReleaseLeaseGuard,
    fence: &PublisherFenceGuard,
) -> Result<()> {
    let (owner, token) = origin.publisher_controls()?;
    let owner = checked_lease_owner(owner)?;
    if !lease.is_owner(owner.as_deref()) || fence.owner() != lease.owner {
        return Err(Error::new(format!(
            "publisher session lost claim ownership: expected {}, observed {}",
            lease.owner,
            owner.as_deref().unwrap_or("absent")
        )));
    }
    let observed = fence_from_observed_token(origin, fence, token)?;
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

/// Delete only this process's exact token.
pub fn release_publisher_fence(
    origin: &dyn Origin,
    guard: &PublisherFenceGuard,
) -> Result<FenceRelease> {
    match publisher_fence_for_guard(origin, guard)? {
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
    let written = write_control_refs(
        origin,
        &guard.owner,
        vec![ControlRefChange {
            reference: PUBLISHER_FENCE_REF.into(),
            before: Some(guard.token.clone()),
            after: None,
        }],
    )?;
    match publisher_fence_for_guard(origin, guard)? {
        None => Ok(FenceRelease::Released),
        Some(current) if current.token != guard.token => Ok(FenceRelease::AlreadySuperseded),
        Some(_) => Err(Error::new(format!(
            "CAS release of publisher fence failed: {}",
            ref_write_description(written)
        ))),
    }
}

/// Withdraw the release tag and unlock the session.
pub fn withdraw_tag_then_unlock(
    git: &dyn GitRunner,
    origin: &dyn Origin,
    tag: &str,
    owner: &str,
    lease: &ReleaseLeaseGuard,
    fence: &PublisherFenceGuard,
) -> Result<LeaseRelease> {
    assert_publisher_session(origin, lease, fence)?;
    delete_owned_release_tag(git, origin, tag, owner, lease, fence)?;
    assert_publisher_session(origin, lease, fence)?;
    release_completed_publisher_session(origin, owner, fence)
}

/// Final unlock deletes the persistent owner and the process token in one atomic ref transaction.
pub fn release_completed_publisher_session(
    origin: &dyn Origin,
    expected_owner: &str,
    guard: &PublisherFenceGuard,
) -> Result<LeaseRelease> {
    let expected_owner = expected_owner.to_ascii_lowercase();
    let (owner, token) =
        crate::cut_timing::measure("unlock read controls", || origin.publisher_controls())?;
    let owner = checked_lease_owner(owner)?;
    let fence = crate::cut_timing::measure("unlock read fence", || {
        fence_from_observed_token(origin, guard, token)
    })?;
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
    let written = crate::cut_timing::measure("unlock native writer", || {
        write_control_refs(
            origin,
            &expected_owner,
            vec![
                ControlRefChange {
                    reference: RELEASE_LEASE_REF.into(),
                    before: Some(expected_owner.clone()),
                    after: None,
                },
                ControlRefChange {
                    reference: PUBLISHER_FENCE_REF.into(),
                    before: Some(guard.token().into()),
                    after: None,
                },
            ],
        )
    })?;
    let (owner_now, token_now) =
        crate::cut_timing::measure("unlock verify controls", || origin.publisher_controls())?;
    let owner_now = checked_lease_owner(owner_now)?;
    let fence_now = crate::cut_timing::measure("unlock verify fence", || {
        fence_from_observed_token(origin, guard, token_now)
    })?;
    match (owner_now.as_deref(), fence_now.as_ref()) {
        (None, None) => Ok(LeaseRelease::Released),
        (Some(_), None) => Ok(LeaseRelease::AlreadySuperseded),
        (Some(owner), Some(current))
            if (owner != expected_owner || current.token != guard.token())
                && current.owner == owner =>
        {
            Ok(LeaseRelease::AlreadySuperseded)
        }
        _ => Err(Error::new(format!(
            "atomic final unlock failed or left inconsistent refs: {}; owner {}, fence {}",
            ref_write_description(written),
            owner_now.as_deref().unwrap_or("absent"),
            fence_now
                .as_ref()
                .map_or("absent", |current| current.token.as_str())
        ))),
    }
}

/// Pure `PublishChecked` seam.
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

/// CAS-safe unlock-only crash convergence, valid exclusively after every publishing step is journaled.
pub fn release_completed_release_lease(
    origin: &dyn Origin,
    expected_owner: &str,
) -> Result<LeaseRelease> {
    let expected_owner = expected_owner.to_ascii_lowercase();
    match release_lease_owner(origin)? {
        None => return Ok(LeaseRelease::AlreadyAbsent),
        Some(owner) if owner != expected_owner => return Ok(LeaseRelease::AlreadySuperseded),
        Some(_) => {}
    }
    let written = write_control_refs(
        origin,
        &expected_owner,
        vec![ControlRefChange {
            reference: RELEASE_LEASE_REF.into(),
            before: Some(expected_owner.clone()),
            after: None,
        }],
    )?;
    let now = release_lease_owner(origin)?;
    if now.is_none() {
        return Ok(LeaseRelease::Released);
    }
    if now.as_deref() != Some(expected_owner.as_str()) {
        return Ok(LeaseRelease::AlreadySuperseded);
    }
    Err(Error::new(format!(
        "CAS release of {RELEASE_LEASE_REF} failed: {}; current owner is {}",
        ref_write_description(written),
        now.as_deref().unwrap_or("absent")
    )))
}

/// Replay unlock-only cleanup for a completed session when no fence guard is held.
pub fn release_completed_session_without_guard(
    origin: &dyn Origin,
    expected_owner: &str,
) -> Result<LeaseRelease> {
    let expected_owner = expected_owner.to_ascii_lowercase();
    let owner = release_lease_owner(origin)?;
    let observed_fence = publisher_fence(origin)?;
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
        (_, None) => release_completed_release_lease(origin, &expected_owner),
    }
}

/// Pipeline steps in execution order, as journaled.
pub const STEPS: [&str; 6] = ["lock", "sign", "selfcheck", "tag", "publish", "unlock"];

/// The one journal format this cutter reads and writes.
pub const JOURNAL_FORMAT: u32 = 12;

/// The part of a cut journal every format has written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalHeader {
    pub format: u32,
    pub version: String,
    pub build_number: u64,
    pub commit: String,
    pub done: Vec<String>,
}

impl JournalHeader {
    /// Read the header.
    pub fn read(path: &Path) -> Result<Option<Self>> {
        Ok(Self::read_classified(path)?)
    }

    /// [`JournalHeader::read`], telling a read that failed from a file that is no cut journal.
    pub fn read_classified(path: &Path) -> std::result::Result<Option<Self>, JournalUnread> {
        match read_journal_text(path)? {
            Some(text) => Self::parse(&text, path)
                .map(Some)
                .map_err(JournalUnread::Invalid),
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

    /// The cut released its lease and fence.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.done.iter().any(|step| step == "unlock")
    }

    /// Where an unfinished cut stands, for a refusal.
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

/// Why a journal file that is there was not read.
#[derive(Debug)]
pub enum JournalUnread {
    /// The read of a regular file failed (I/O, permissions), or the path could not be looked
    /// at; a retry may get through.
    Read(Error),
    /// The path holds no journal this cutter reads: no regular file (a directory, a FIFO, a
    /// symbolic link, which no journal read follows), not UTF-8, larger than any journal, or text
    /// that does not parse or validate. Every retry reads the same.
    Invalid(Error),
}

impl From<JournalUnread> for Error {
    fn from(unread: JournalUnread) -> Self {
        match unread {
            JournalUnread::Read(error) | JournalUnread::Invalid(error) => error,
        }
    }
}

/// A journal file's text, `None` when there is none. The read refuses what is no regular file
/// without blocking on it (a FIFO opens non-blocking) and follows no symbolic link; a failed read
/// is told apart by what the path holds, looked at without following a link: anything but a
/// regular file is [`JournalUnread::Invalid`], as every retry finds it the same.
fn read_journal_text(path: &Path) -> std::result::Result<Option<String>, JournalUnread> {
    match crate::durable::read_regular_text(path, 4 * 1024 * 1024) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => match e.kind() {
            std::io::ErrorKind::InvalidData | std::io::ErrorKind::FileTooLarge => Err(
                JournalUnread::Invalid(Error::new(format!("read {}: {e}", path.display()))),
            ),
            _ => match std::fs::symlink_metadata(path).map(|meta| meta.file_type()) {
                Ok(kind) if !kind.is_file() => {
                    let what = if kind.is_dir() {
                        "a directory"
                    } else if kind.is_symlink() {
                        "a symbolic link, which no journal read follows"
                    } else {
                        "a FIFO, socket or device"
                    };
                    Err(JournalUnread::Invalid(Error::new(format!(
                        "{} is {what}, not a regular file",
                        path.display()
                    ))))
                }
                _ => Err(JournalUnread::Read(Error::new(format!(
                    "read {}: {e}",
                    path.display()
                )))),
            },
        },
    }
}

/// The cut journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    pub format: u32,
    pub version: String,
    pub build_number: u64,
    pub commit: String,
    #[serde(default)]
    pub min_build: Option<u64>,
    #[serde(default)]
    pub artifact: Option<String>,
    #[serde(default)]
    pub macho_sha256: Option<String>,
    #[serde(default)]
    pub linux: Option<buildplan::linux::Handoff>,
    /// The Windows handoff (`--windows-artifacts`), its zip frozen at the sign step. Absent
    /// from a journal that declared none, so such a journal's bytes are as they were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<buildplan::windows::Handoff>,
    #[serde(default)]
    pub manifest_signed: bool,
    #[serde(default)]
    pub signature_required: bool,
    #[serde(default)]
    pub signature_pubkey: Option<String>,
    #[serde(default)]
    pub verify_pubkey: Option<String>,
    #[serde(default)]
    pub signature_machine_id: Option<String>,
    #[serde(default)]
    pub release_id: Option<u64>,
    #[serde(default)]
    pub release_intent: bool,
    #[serde(default)]
    pub upload_intents: Vec<String>,
    /// A private draft's journal only: the release its withdrawal issues a DELETE for, journaled
    /// before the DELETE goes out, so a retry reads that release's absence (a 404 by its id that
    /// the releases listing agrees with) as the withdrawal's own ([`draft::discard`]). A cut
    /// journal names none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_intent: Option<u64>,
    #[serde(default)]
    pub done: Vec<String>,
}

impl Journal {
    /// Read this cutter's journal.
    pub fn load(path: &Path) -> Result<Option<Journal>> {
        read_journal_text(path)?
            .map(|text| Self::from_text(&text, path))
            .transpose()
    }

    /// Parse `text`, read from `path`, as this cutter's journal: a journal of another format, or
    /// one that does not parse or validate, is refused.
    fn from_text(text: &str, path: &Path) -> Result<Journal> {
        let header = JournalHeader::parse(text, path)?;
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
        Self::parse_loaded(text, path)
    }

    fn parse_loaded(text: &str, path: &Path) -> Result<Journal> {
        let journal: Journal = aterm_toml::from_str(text)
            .map_err(|e| Error::new(format!("parse {}: {e}", path.display())))?;
        journal.validate()?;
        Ok(journal)
    }

    /// This cutter's journal, or `Ok(None)` for no journal and for another cutter's format.
    pub fn load_if_ours(path: &Path) -> Result<Option<Journal>> {
        let Some(text) = read_journal_text(path)? else {
            return Ok(None);
        };
        let header = JournalHeader::parse(&text, path)?;
        if header.format == JOURNAL_FORMAT {
            Self::parse_loaded(&text, path).map(Some)
        } else {
            Ok(None)
        }
    }

    /// Persist atomically (temp + rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let text = aterm_toml::to_string(self)
            .map_err(|e| Error::new(format!("serialize journal: {e}")))?;
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
        let commit_tmp = (|| -> Result<()> {
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
            })
        })();
        if let Err(err) = commit_tmp {
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
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
        if let Some(windows) = &self.windows {
            windows.validate_identity(
                &self.version,
                self.build_number,
                &self.commit,
                self.is_done("sign"),
            )?;
        }
        validate_min_build(self.min_build, self.build_number, "journaled build")?;
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
            if let Some(verify_pubkey) = self.verify_pubkey.as_deref() {
                canonical_update_pubkey(verify_pubkey)?;
            }
            if self
                .signature_machine_id
                .as_deref()
                .is_some_and(|id| id.trim().is_empty())
            {
                return Err(Error::new(
                    "signed release journal carries an empty signature_machine_id",
                ));
            }
        } else if self.signature_pubkey.is_some()
            || self.verify_pubkey.is_some()
            || self.signature_machine_id.is_some()
            || self.manifest_signed
        {
            return Err(Error::new(
                "release journal carries signature bytes/key while signature_required is false",
            ));
        }
        if self.is_done("sign") && self.signature_required && !self.manifest_signed {
            return Err(Error::new(
                "release journal marks sign complete without its required manifest signature",
            ));
        }
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
        if let Some(id) = self.delete_intent
            && (id == 0
                || !self.release_intent
                || self.release_id.is_some_and(|bound| bound != id)
                || !self.done.is_empty())
        {
            return Err(Error::new(
                "release journal carries a delete intent that is not its own private draft's: \
                 zero, with no release intent, another release than the one it binds, or in a \
                 cut journal",
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

    /// The first [`STEPS`] entry not yet journaled.
    pub fn first_incomplete(&self) -> Option<&'static str> {
        STEPS.iter().copied().find(|step| !self.is_done(step))
    }

    /// Record a completed step and persist immediately.
    pub fn mark(&mut self, step: &str, path: &Path) -> Result<()> {
        if !self.is_done(step) {
            self.done.push(step.to_string());
        }
        self.save(path)
    }
}

/// Admission decision for a non-idempotent remote POST whose response may be lost.
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

/// An absent listing is destructive-cleanup authority only when a current durable journal proves no create POST was issued.
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

/// Decide whether an existing draft release under this tag is safe to delete.
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
pub(crate) struct DurablePostPermit(());

impl Drop for DurablePostPermit {
    fn drop(&mut self) {}
}

struct OneShotPost(aterm_update_core::github_rest::Prepared);

const CURL_EXIT_FAILED_INIT: i32 = 2;

const fn transport_never_started(out: &RunOut) -> bool {
    out.status == CURL_EXIT_FAILED_INIT
}

const CURL_EXIT_HTTP_ERROR: i32 = 22;

fn server_refused_without_creating(out: &RunOut) -> bool {
    if out.status != CURL_EXIT_HTTP_ERROR {
        return false;
    }
    http_status_from_curl_failure(&out.stderr_utf8()).is_some_and(|code| (400..500).contains(&code))
}

fn http_status_from_curl_failure(stderr: &str) -> Option<u16> {
    let tail = stderr.rsplit_once("returned error: ")?.1;
    let len = tail.bytes().take_while(u8::is_ascii_digit).count();
    tail[..len].parse().ok()
}

/// What a failed one-shot POST proves about the object it asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PostOutcome {
    NeverSent,
    Refused,
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

/// What `upload_channel_asset` does next, once its POST has finished and [`CutCtx::settle_upload_intent`] has spent the intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AfterUploadPost {
    NeverSent,
    Refused,
    Converge { journal: bool },
    Ambiguous,
}

/// Choose the [`AfterUploadPost`] recovery action from the upload `POST` outcome and remote asset probe.
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

const HTTP_UNPROCESSABLE_CONTENT: u16 = 422;

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

fn release_post_failure_detail(out: &RunOut, posted_body_chars: usize) -> String {
    use std::fmt::Write as _;
    let stderr = out.stderr_utf8();
    let mut detail = stderr.trim().to_string();
    if http_status_from_curl_failure(&stderr) == Some(HTTP_UNPROCESSABLE_CONTENT) {
        let limit = changelog::GITHUB_RELEASE_BODY_LIMIT;
        let _ = write!(
            detail,
            " — {HTTP_UNPROCESSABLE_CONTENT} is GitHub understanding the request and refusing \
             its CONTENT. The release body this POST carried was {posted_body_chars} characters \
             against the {limit}-character release-body limit, which is "
        );
        if posted_body_chars > limit {
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
        let _ = write!(detail, " — GitHub said: {said}");
    }
    detail
}

fn release_body_post_failure_detail(out: &RunOut, body: &ReleaseBody) -> String {
    release_post_failure_detail(out, changelog::github_length(body))
}

fn release_body_for_post(label: &str, path: &Path) -> Result<ReleaseBody> {
    let raw = gates::read_bounded_string(path, 64 << 20)
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

/// A release body read and bounded at the post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReleaseBody(String);

impl std::ops::Deref for ReleaseBody {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

fn release_create_payload(tag: &str, title: &str, body: &ReleaseBody) -> Result<Vec<u8>> {
    #[derive(Serialize)]
    struct Payload<'a> {
        tag_name: &'a str,
        name: &'a str,
        body: &'a str,
        draft: bool,
        prerelease: bool,
    }
    aterm_json::to_vec(&Payload {
        tag_name: tag,
        name: title,
        body,
        draft: true,
        prerelease: false,
    })
    .map_err(|error| Error::new(format!("serialize release request: {error}")))
}

impl OneShotPost {
    fn prepare_json(path: &str, payload: &[u8]) -> Result<Self> {
        crate::github::prepare(
            crate::github::OneShotUrl::Api(path),
            aterm_update_core::github_rest::Body::Json(payload),
        )
        .map(Self)
    }

    fn prepare_binary(path: &str, file: &Path) -> Result<Self> {
        crate::github::prepare(
            crate::github::OneShotUrl::Uploads(path),
            aterm_update_core::github_rest::Body::File(file),
        )
        .map(Self)
    }

    /// curl's argv as a process listing would show it: the whole security- and
    /// memory-relevant surface of a one-shot POST.
    #[cfg(test)]
    fn argv(&self) -> Vec<String> {
        self.0.argv()
    }

    fn issue(self, permit: DurablePostPermit) -> Result<RunOut> {
        let out = crate::github::run_once(self.0);
        drop(permit);
        out
    }
}

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

/// Resolve the floor for a newly claimed build.
pub fn effective_min_build(
    operator: Option<u64>,
    newest_channel: Option<u64>,
    claimed_build: u64,
) -> Result<Option<u64>> {
    let floor = operator.unwrap_or(0).max(newest_channel.unwrap_or(0));
    validate_min_build(Some(floor), claimed_build, "newly claimed build")
}

/// Late race guard.
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

/// The roster ratchet, sibling of [`channel_floor_covered`].
pub fn roster_floor_covered(carried: Option<u64>, newest_channel: Option<u64>) -> Result<()> {
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

/// One GitHub release asset relevant to the head-signature replay.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AppcastAsset {
    pub id: u64,
    pub name: String,
}

/// The appcast assets on one release.
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

pub use aterm_update_core::tag::TagKind;

/// Classify one release tag by [`aterm_update_core::tag::parse_release_tag`].
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

/// Parse the release protocol's canonical `vMAJOR.MINOR.PATCH` tag into a numeric order.
pub fn canonical_channel_tag_order(tag: &str) -> Result<(u64, u64, u64)> {
    let not_canonical = || {
        Error::new(format!(
            "published appcast tag {tag:?} is not canonical vMAJOR.MINOR.PATCH"
        ))
    };
    let TagKind::Candidate(components) = parse_release_tag(tag)? else {
        return Err(not_canonical());
    };
    if aterm_update_core::tag::canonical_version(tag, &components).is_none() {
        return Err(not_canonical());
    }
    let [major, minor, patch] = components.as_slice() else {
        return Err(not_canonical());
    };
    Ok((*major, *minor, *patch))
}

/// The canonical version string carried by a canonical release tag.
pub fn canonical_channel_tag_version(tag: &str) -> Result<String> {
    let (major, minor, patch) = canonical_channel_tag_order(tag)?;
    Ok(format!("{major}.{minor}.{patch}"))
}

fn appcast_listing_row(release: &aterm_json::Value) -> String {
    let assets: Vec<aterm_json::Value> = release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| {
            matches!(
                a["name"].as_str(),
                Some("aterm-appcast.toml" | "aterm-appcast.toml.sig")
            )
        })
        .map(|a| {
            aterm_json::Value::Object(aterm_json::Map::from([
                ("id".to_string(), a["id"].clone()),
                ("name".to_string(), a["name"].clone()),
            ]))
        })
        .collect();
    aterm_json::Value::Object(aterm_json::Map::from([
        ("release_id".to_string(), release["id"].clone()),
        ("tag".to_string(), release["tag_name"].clone()),
        ("draft".to_string(), release["draft"].clone()),
        (
            "target_commitish".to_string(),
            release["target_commitish"].clone(),
        ),
        ("assets".to_string(), aterm_json::Value::Array(assets)),
    ]))
    .to_string()
}

/// Parse the bounded GitHub listing [`list_appcast_releases`] reads.
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

fn list_appcast_releases(slug: &str) -> Result<Vec<AppcastRelease>> {
    let listing = release_listing(slug)?;
    parse_appcast_asset_listing(&crate::github::rows(
        listing.iter().map(appcast_listing_row),
    ))
}

const RELEASES_PER_PAGE: usize = 100;

/// Every release object of `slug`, the listing read whole (drafts included when the token
/// can push).
pub(crate) fn release_listing(slug: &str) -> Result<Vec<aterm_json::Value>> {
    crate::github::pages(
        &format!("repos/{slug}/releases?per_page={RELEASES_PER_PAGE}"),
        RELEASES_PER_PAGE,
    )
}

fn update_key_fingerprint(encoded: &str) -> Result<String> {
    let canonical = canonical_update_pubkey(encoded)?;
    let raw = aterm_codec::base64::decode_strict(canonical.as_bytes())
        .map_err(|_| Error::new("canonical update key failed to decode for fingerprint"))?;
    Ok(sha256_bytes(&raw))
}

/// The cut's signing verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignaturePolicy {
    pub required: bool,
    pub pubkey: Option<String>,
}

/// The verdict of a tree with no paper master pinned (a fork).
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

/// What a pipeline entry will still do with the signing material, and so how much of the roster chain it re-proves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterDuty {
    Sign,
    Finish,
}

/// Everything the armed machine-roster tier decides on, bundled so a caller cannot supply four parameters and forget the fifth.
pub struct RosterEvidence<'a> {
    pub master_pubkeys: &'a [&'a str],
    pub roster: Option<&'a machines::RosterDocument>,
    pub declared_machine_id: Option<&'a str>,
    pub now_unix: i64,
    pub duty: RosterDuty,
}

/// The two-state signing gate, chosen by the paper master anchor alone.
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
             key: cut with --release-credentials ~/.aterm/release-ring/release-credentials.toml (`targo \
             --unverified ship provision --id <id>` writes it)",
        ));
    };
    let material = canonical_update_pubkey(material)?;
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
             \"<path>/aterm-machines.toml\"` (its .sig beside it; a leading `~/` is this Mac's \
             home), or move the profile aside and re-run `targo --unverified ship provision \
             --id <id>`, which writes it",
        ));
    };
    let who = machines::authorize_cut(
        evidence.master_pubkeys,
        document.bytes.clone(),
        &document.signature,
        &material,
        evidence.now_unix,
    )?;
    if let Some(declared) = evidence.declared_machine_id
        && declared != who.machine_id
    {
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

/// Decode and re-emit the updater Ed25519 key so journal/config comparisons use one canonical identity rather than textual base64 aliases.
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

/// Verify raw detached Ed25519 bytes against the canonical/persisted channel key.
pub fn verify_detached_manifest_signature(
    encoded_pubkey: &str,
    manifest: &[u8],
    signature: &[u8],
) -> Result<()> {
    let canonical = canonical_update_pubkey(encoded_pubkey)?;
    let pubkey = aterm_codec::base64::decode_strict(canonical.as_bytes())
        .map_err(|_| Error::new("canonical update public key failed to decode"))?;
    let pubkey = <&[u8; 32]>::try_from(pubkey.as_slice())
        .map_err(|_| Error::new("canonical update public key is not 32 bytes"))?;
    let signature = <&[u8; 64]>::try_from(signature).map_err(|_| {
        Error::new(format!(
            "manifest signature is {} bytes, not an Ed25519 64-byte signature",
            signature.len()
        ))
    })?;
    if astream_aead::signature::verify(pubkey, manifest, signature) {
        Ok(())
    } else {
        Err(Error::new(
            "manifest signature does not verify under the channel public key",
        ))
    }
}

/// The exact-head signature check, with the asset download injected so the decision runs without GitHub.
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
pub fn verify_live_channel_head_signature(
    slug: &str,
    head_tag: &str,
    head_manifest: &[u8],
    local_head_signature: Option<&[u8]>,
    journal_pubkey: Option<&str>,
) -> Result<bool> {
    let Some(journal_pubkey) = journal_pubkey else {
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

struct SigningMaterial {
    pubkey: String,
}

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

fn resolve_apple_tier(
    team_id: &str,
    credentials: Option<&sign::ReleaseCredentials>,
) -> Result<sign::AppleTier> {
    if !aterm_update_core::pins::anchor_active(team_id) {
        return Ok(sign::AppleTier::Inactive);
    }
    let tier = sign::resolve_apple_tier(team_id, credentials).map_err(Error::new)?;
    println!("{}", tier.describe());
    Ok(tier)
}

/// The tier a resume must resolve.
pub fn resume_apple_tier(
    team_id: &str,
    journal: &Journal,
    credentials: Option<&sign::ReleaseCredentials>,
) -> Result<sign::AppleTier> {
    if journal.is_done("sign") {
        return Ok(sign::AppleTier::Inactive);
    }
    resolve_apple_tier(team_id, credentials)
}

const MAX_SMALL_RELEASE_ASSET_BYTES: u64 = 256 * 1024;

/// Immutable GitHub release capability.
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
            let id = (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
                .then(|| id.parse::<u64>().ok())
                .flatten()
                .ok_or_else(|| {
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

/// The identity rows of `releases` ([`crate::github::identity_row`] each), as
/// [`parse_release_object_identity_rows`] reads them.
pub(crate) fn release_identity_rows<'a>(
    releases: impl IntoIterator<Item = &'a aterm_json::Value>,
) -> String {
    crate::github::rows(releases.into_iter().map(crate::github::identity_row))
}

/// Exhaustively resolve a tag to release objects.
pub fn release_objects_by_tag(slug: &str, tag: &str) -> Result<Vec<ReleaseObjectIdentity>> {
    let listing = release_listing(slug)?;
    let rows = parse_release_object_identity_rows(&release_identity_rows(&listing))?;
    Ok(rows
        .into_iter()
        .filter(|release| release.tag == tag)
        .collect())
}

/// Whether `slug`'s releases listing, drafts included, holds release `id`, under any tag.
pub(crate) fn release_listed(slug: &str, id: u64) -> Result<bool> {
    let listing = release_listing(slug)?;
    Ok(
        parse_release_object_identity_rows(&release_identity_rows(&listing))?
            .iter()
            .any(|release| release.id == id),
    )
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
    let Some(object) = crate::github::get_optional(&format!("repos/{slug}/releases/{id}"))
        .map_err(|e| Error::new(format!("read exact GitHub release ID {id} failed: {e}")))?
    else {
        return Ok(None);
    };
    let rows = parse_release_object_identity_rows(&release_identity_rows([&object]))?;
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
    pub release: Option<ReleaseObjectIdentity>,
    pub asset: Option<(u64, u64)>,
}

/// The release-object identity and the exact-name asset binding from one read of `repos/{slug}/releases/{id}`.
pub fn release_object_and_asset_identity(
    slug: &str,
    release_id: u64,
    name: &str,
) -> Result<ReleaseObjectAndAsset> {
    let Some(object) = crate::github::get_optional(&format!("repos/{slug}/releases/{release_id}"))?
    else {
        return Ok(ReleaseObjectAndAsset {
            release: None,
            asset: None,
        });
    };
    let asset_rows = crate::github::asset_rows(&object);
    let rows = parse_release_object_identity_rows(&release_identity_rows([&object]))?;
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
        observed == expected
    }
}

/// Revalidate the complete release-object snapshot captured around an exact-ID asset download.
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

/// Bound every asset ever captured in memory.
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

/// The bracketed transfer.
pub(crate) fn download_release_asset_with_identity_and_recheck(
    slug: &str,
    name: &str,
    before: (u64, u64),
    mut recheck: impl FnMut() -> Result<(u64, u64)>,
) -> Result<Vec<u8>> {
    let limit = validate_small_release_asset_size(name, before.1)?;
    let temp_dir = private_release_asset_temp(&std::env::temp_dir())?;
    let temp_asset = temp_dir.path().join("asset");
    let bytes = finish_private_asset_temp(
        temp_dir,
        (|| {
            crate::github::download_asset(slug, before.0, &temp_asset, before.1)?;
            let bytes = fs::read(&temp_asset).map_err(|error| {
                Error::new(format!("read exact GitHub asset-ID {}: {error}", before.0))
            })?;
            if bytes.len() > limit || u64::try_from(bytes.len()).ok() != Some(before.1) {
                return Err(Error::new(format!(
                    "release asset {name} API size {} differs from bounded download size {}",
                    before.1,
                    bytes.len()
                )));
            }
            Ok(bytes)
        })(),
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

const UPDATER_MAX_DMG_BYTES: u64 = aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND;

const LEAN_DMG_CEILING_BYTES: u64 = 200 * 1000 * 1000;

fn lean_dmg_overweight(size: u64) -> Option<String> {
    (size > LEAN_DMG_CEILING_BYTES).then(|| {
        format!(
            "{:.2} GB ({size} bytes), over the {} MB lean ceiling",
            size as f64 / 1e9,
            LEAN_DMG_CEILING_BYTES / 1_000_000,
        )
    })
}

/// Refuse a just-packaged macOS image that is not the lean download.
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

fn recover_lean_dmg_to<F>(destination: &Path, download: F) -> Result<VerifiedReleaseAsset>
where
    F: FnOnce(&Path) -> Result<VerifiedReleaseAsset>,
{
    let asset = download(destination)?;
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

fn private_release_asset_temp(parent: &Path) -> Result<PrivateTempDir> {
    let parent = aterm_tempfile::quiet_in(parent);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = RELEASE_ASSET_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    PrivateTempDir::create(parent.join(format!(
        "aterm-release-asset-{}-{nonce}-{sequence}.noindex",
        std::process::id()
    )))
}

fn finish_private_asset_temp<T>(temp: PrivateTempDir, result: Result<T>) -> Result<T> {
    let cleanup = temp.cleanup();
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(error), Err(cleanup)) => {
            Err(Error::new(format!("{error}; cleanup failed: {cleanup}")))
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
            let uint = |s: &str| {
                (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| s.parse::<u64>().ok())
                    .flatten()
            };
            Some(
                uint(id)
                    .zip(uint(size))
                    .ok_or_else(|| Error::new("GitHub release asset has non-numeric id/size")),
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
    let release = crate::github::get(&format!("repos/{slug}/releases/{release_id}"))?;
    parse_release_asset_identity_rows(
        &crate::github::asset_rows(&release),
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
    let release = crate::github::get(&format!("repos/{slug}/releases/{release_id}"))?;
    let mut inventory = Vec::new();
    for (index, line) in crate::github::asset_rows(&release).lines().enumerate() {
        let mut fields = line.split('\t');
        let (Some(name), Some(id), Some(size), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(Error::new(format!(
                "malformed release inventory row {}",
                index + 1
            )));
        };
        let uint = |s: &str, msg: &str| {
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse::<u64>().ok())
                .flatten()
                .ok_or_else(|| Error::new(msg))
        };
        inventory.push(ReleaseAssetInventoryEntry {
            name: name.to_string(),
            id: uint(id, "release inventory asset ID is non-numeric")?,
            size: uint(size, "release inventory asset size is non-numeric")?,
        });
    }
    inventory.sort_unstable();
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

/// Refuse a manifest that still names the retired Intel DMG pair.
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
    let verified = verify_release_asset_bytes(slug, release_id, name, local, before)?;
    if release_asset_identity_for_release_id(slug, release_id, name)? != before {
        return Err(Error::new(format!(
            "release ID {release_id} asset {name} identity changed during exact-ID verification"
        )));
    }
    Ok(verified)
}

/// The caller brackets this transfer with either the asset identity or the
/// complete immutable-ID inventory. Both detect replacement during the proof.
fn verify_release_asset_bytes(
    slug: &str,
    release_id: u64,
    name: &str,
    local: &Path,
    before: (u64, u64),
) -> Result<VerifiedReleaseAsset> {
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
    let temp_dir = private_release_asset_temp(&std::env::temp_dir())?;
    let temp_asset = temp_dir.path().join("asset");
    finish_private_asset_temp(
        temp_dir,
        (|| {
            crate::github::download_asset(slug, before.0, &temp_asset, before.1)?;
            let downloaded_size = fs::metadata(&temp_asset)
                .map_err(|error| Error::new(format!("stat downloaded release asset: {error}")))?
                .len();
            let sha256 = dmg::sha256_file(&temp_asset)?;
            if downloaded_size != before.1 || dmg::sha256_file(local)? != sha256 {
                return Err(Error::new(format!(
                    "release ID {release_id} asset {name} bytes differ from the local \
                     self-checked artifact"
                )));
            }
            Ok(VerifiedReleaseAsset {
                id: before.0,
                size: before.1,
                sha256,
            })
        })(),
    )
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
    let temp_dir = private_release_asset_temp(temp_parent)?;
    let temp_asset = temp_dir.path().join("asset");
    let result = (|| -> Result<VerifiedReleaseAsset> {
        crate::github::download_asset(slug, id, &temp_asset, size)?;
        let downloaded_size = fs::metadata(&temp_asset)
            .map_err(|error| Error::new(format!("stat downloaded release asset: {error}")))?
            .len();
        let digest = dmg::sha256_file(&temp_asset)?;
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
        let after = release_asset_identity_for_release_id(slug, release_id, name)?;
        if after != (id, size) {
            return Err(Error::new(format!(
                "release asset {name} identity changed after exact-ID download and digest"
            )));
        }
        if let Some(destination) = retain_at {
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
    finish_private_asset_temp(temp_dir, result)
}

/// The anchor the shipped binary must prove it compiled in.
pub fn expected_embedded_update_pin(master_pubkeys: &[&str]) -> Result<Option<String>> {
    master_pubkeys
        .first()
        .map(|master| update_key_fingerprint(master))
        .transpose()
}

fn roster_now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(i64::MAX, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The complete pre-claim signing verdict.
#[derive(Debug)]
pub struct SigningVerdict {
    pub policy: SignaturePolicy,
    pub attribution: Option<roster::Attribution>,
    pub roster: Option<machines::RosterDocument>,
}

impl SigningVerdict {
    fn machine_id(&self) -> Option<String> {
        self.attribution.as_ref().map(|who| who.machine_id.clone())
    }

    fn cut_attribution(self) -> CutAttribution {
        CutAttribution {
            machine_id: self.machine_id(),
            attribution: self.attribution,
            roster: self.roster,
        }
    }
}

struct CutAttribution {
    machine_id: Option<String>,
    attribution: Option<roster::Attribution>,
    roster: Option<machines::RosterDocument>,
}

impl CutAttribution {
    const fn none() -> Self {
        Self {
            machine_id: None,
            attribution: None,
            roster: None,
        }
    }
}

/// May a resume that re-authorized as `observed` continue a cut the journal says was started by `journaled`? Only as the same machine.
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

fn preflight_signature_policy(
    creds: Option<&sign::ReleaseCredentials>,
    duty: RosterDuty,
) -> Result<SigningVerdict> {
    let home = aterm_types::dirs::home_dir();
    signing_verdict(
        creds,
        &SigningAnchors {
            home: home.as_deref(),
            master_pubkeys: aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
            identity_path: machines::conventional_identity_path().as_deref(),
            now_unix: roster_now_unix(),
            duty,
        },
    )
}

const fn roster_duty(build_done: bool) -> RosterDuty {
    if build_done {
        RosterDuty::Finish
    } else {
        RosterDuty::Sign
    }
}

/// The anchors and ambient inputs [`signing_verdict`] resolves against.
pub struct SigningAnchors<'a> {
    pub home: Option<&'a Path>,
    pub master_pubkeys: &'a [&'a str],
    pub identity_path: Option<&'a Path>,
    pub now_unix: i64,
    pub duty: RosterDuty,
}

/// [`signing_verdict`]'s refusal of an armed cut whose anchors name no home.
#[cfg(unix)]
pub(crate) const NO_HOME_CONTEXT: &str = "current signing authority needs an explicit home context";

pub fn signing_verdict(
    creds: Option<&sign::ReleaseCredentials>,
    anchors: &SigningAnchors<'_>,
) -> Result<SigningVerdict> {
    let armed = !anchors.master_pubkeys.is_empty() && anchors.duty == RosterDuty::Sign;
    #[cfg(not(unix))]
    if armed {
        return Err(Error::new(
            "current roster signing authority is unavailable on this platform",
        ));
    }
    #[cfg(unix)]
    let current = if armed {
        let home = anchors.home.ok_or_else(|| Error::new(NO_HOME_CONTEXT))?;
        let named = creds
            .and_then(sign::ReleaseCredentials::machine_roster)
            .ok_or_else(|| Error::new("current signing authority needs a named roster pair"))?;
        // A configured missing pair remains an error, not permission to fall back.
        if machines::current_roster::read_local_candidate_with(named, anchors.master_pubkeys)?
            .is_none()
        {
            return Err(Error::new(
                "the profile's named public roster pair is absent",
            ));
        }
        let canonical = home.join(atpkg_keys::provision::RELEASE_ROSTER_REL);
        Some(machines::current_roster::resolve(
            home,
            &[named, &canonical],
            anchors.master_pubkeys,
            Ok(None),
            aterm_secrets::client::current_public_roster,
        )?)
    } else {
        None
    };
    #[cfg(unix)]
    let document = current
        .as_ref()
        .map(machines::current_roster::CurrentRoster::document);
    #[cfg(not(unix))]
    let document = None;
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
    #[cfg(unix)]
    if let Some(current) = current.as_ref() {
        current.recheck(
            anchors.home.expect("current authority has a home"),
            anchors.master_pubkeys,
        )?;
    }
    Ok(SigningVerdict {
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
    let signature = manifest.with_extension("toml.sig");
    let manifest_bytes = gates::read_bounded_bytes(manifest, 16 << 20)
        .map_err(|error| Error::new(format!("read {}: {error}", manifest.display())))?;
    let signature_bytes = creds.sign(&manifest_bytes).map_err(Error::new)?;
    crate::durable::write_fresh(&signature, &signature_bytes)
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

fn sign_ctx_manifest(
    ctx: &CutCtx,
    manifest: &Path,
    validate: impl FnOnce(&CutCtx) -> Result<()>,
) -> Result<PathBuf> {
    let _timing = crate::cut_timing::Timer::new("manifest signature");
    validate(ctx)?;
    sign_manifest_with_policy(ctx, manifest)
}

/// Assemble the manifest, stamp the attribution into it, prove the bytes and write them, in that order.
pub fn stage_manifest(
    dist: &Path,
    inputs: &manifest_out::ManifestInputs<'_>,
    who: Option<&roster::Attribution>,
    linux: Option<&buildplan::linux::Handoff>,
    windows: Option<&buildplan::windows::Handoff>,
) -> Result<PathBuf> {
    let mut manifest = manifest_out::build(inputs);
    if let Some(linux) = linux {
        linux.verify_staged(dist, inputs.version, inputs.build_number, inputs.commit)?;
        linux.stamp(&mut manifest)?;
    }
    if let Some(windows) = windows {
        windows.verify_staged(dist, inputs.version, inputs.build_number, inputs.commit)?;
        windows.stamp(&mut manifest)?;
    }
    if let Some(who) = who {
        machines::attribute(&mut manifest, who);
    }
    manifest_out::write(dist, &manifest)
}

/// Stage the master-signed roster beside the appcast, so a client can fetch the document that authorizes the signature it checks.
pub fn stage_roster_assets(dist: &Path, document: Option<&machines::RosterDocument>) -> Result<()> {
    let Some(document) = document else {
        return Ok(());
    };
    let roster = dist.join(roster::ROSTER_ASSET);
    crate::provision::publish_proven_pair(&roster, &document.bytes, &document.signature)
        .map_err(|e| Error::new(format!("stage {}: {e}", roster.display())))?;
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

/// Validate a published release's exact appcast pair before a lost machine's recovery reconstructs a journal from it.
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

/// The monotonic build-number gate.
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

/// Everything the pipeline steps share, resolved once up front.
pub struct CutCtx {
    pub credentials: Option<sign::ReleaseCredentials>,
    pub apple: sign::AppleTier,
    pub repo: PathBuf,
    pub tree: PathBuf,
    pub dist: PathBuf,
    pub journal_path: PathBuf,
    pub slug: String,
    pub version: String,
    pub tag: String,
    pub build: u64,
    pub commit: String,
    pub min_build: Option<u64>,
    pub artifact: Option<String>,
    pub macho_sha256: Option<String>,
    pub linux: Option<buildplan::linux::Handoff>,
    pub windows: Option<buildplan::windows::Handoff>,
    pub manifest_signed: bool,
    pub signature_required: bool,
    pub signature_pubkey: Option<String>,
    pub verify_pubkey: Option<String>,
    pub signature_machine_id: Option<String>,
    pub attribution: Option<roster::Attribution>,
    pub roster: Option<machines::RosterDocument>,
    pub release_id: Option<u64>,
    pub release_intent: bool,
    pub upload_intents: Vec<String>,
    pub kind: CutKind,
    pub lease: Option<ReleaseLeaseGuard>,
    pub fence: Option<PublisherFenceGuard>,
    pub journal: Option<Journal>,
    /// The sign step adopted the copy `cut --sign` signed of these bytes ([`SignedRecord`]): the
    /// self-check's executions of it and its signing checks ran there, on the same bytes.
    pub adopted: bool,
}

impl CutCtx {
    fn linux_asset_names(&self) -> Vec<String> {
        self.linux.as_ref().map_or_else(Vec::new, |handoff| {
            handoff
                .artifacts
                .iter()
                .map(|artifact| artifact.asset.clone())
                .chain(
                    handoff
                        .verifiers
                        .iter()
                        .map(|verifier| verifier.asset.clone()),
                )
                .collect()
        })
    }

    /// The native assets this cut's handoffs froze: Linux's, then the Windows zip.
    fn handoff_asset_names(&self) -> Vec<String> {
        let mut names = self.linux_asset_names();
        if let Some(windows) = &self.windows {
            names.extend(windows.asset_names());
        }
        names
    }

    fn channel_asset_names(&self) -> Vec<String> {
        let mut names = channel::required_asset_names(
            &self.version,
            self.signature_required,
            self.attaches_roster(),
        );
        names.extend(self.handoff_asset_names());
        names.push(crate::service_qualification::REPORT_ASSET.to_string());
        names.sort();
        names
    }

    fn validate_channel_assets(&self, names: &[String]) -> Result<()> {
        crate::service_qualification::validate_assets(
            names,
            &self.version,
            self.signature_required,
            self.attaches_roster(),
            &self.handoff_asset_names(),
        )
    }

    fn dmg_path(&self) -> PathBuf {
        self.dist.join(channel::dmg_asset_name(&self.version))
    }
    fn zip_path(&self) -> PathBuf {
        self.dist.join(channel::zip_asset_name(&self.version))
    }
    fn verification_pubkey(&self) -> Option<&str> {
        self.verify_pubkey
            .as_deref()
            .or(self.signature_pubkey.as_deref())
    }

    fn app_path(&self) -> PathBuf {
        bundle::staged_app_path(&self.dist, self.build)
    }
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
    fn stable_dmg_path(&self) -> PathBuf {
        self.dist.join(channel::stable_dmg_asset_name())
    }
    fn stable_zip_path(&self) -> PathBuf {
        self.dist.join(channel::stable_zip_asset_name())
    }
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

    /// Persist the durable release intent.
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

    /// Undo [`Self::persist_release_intent`] for the one outcome that proves the create POST created nothing.
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

    fn retract_upload_intent(&mut self, name: &str) -> Result<()> {
        self.upload_intents.retain(|issued| issued != name);
        if let Some(journal) = &mut self.journal {
            journal.upload_intents.retain(|issued| issued != name);
            journal.save(&self.journal_path)?;
        }
        Ok(())
    }

    /// Settle the durable intent of an upload POST that has just finished.
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

    fn channel_asset_paths(&self) -> Vec<PathBuf> {
        self.channel_asset_names()
            .into_iter()
            .map(|name| self.dist.join(name))
            .collect()
    }

    const fn attaches_roster(&self) -> bool {
        self.signature_machine_id.is_some()
    }

    fn expected_embedded_pin(&self) -> Result<Option<String>> {
        expected_embedded_update_pin(aterm_update_core::pins::PAPER_MASTER_PUBKEYS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteAnnotatedTag {
    token: String,
    commit: String,
}

fn remote_annotated_ref(
    origin: &dyn Origin,
    reference: &str,
    label: &str,
) -> Result<Option<RemoteAnnotatedTag>> {
    let Some(token) = origin.get(reference)? else {
        return Ok(None);
    };
    resolve_annotated_token(origin, token, label).map(Some)
}

fn resolve_annotated_token(
    origin: &dyn Origin,
    token: String,
    label: &str,
) -> Result<RemoteAnnotatedTag> {
    if !valid_lease_owner(&token) {
        return Err(Error::new(format!("{label} is not one raw object ID")));
    }
    let token = token.to_ascii_lowercase();
    // Resolve the advertised object, not another read of a mutable ref or peel.
    // Native immutable-object reads disable replacement and lazy fetching.
    origin.import(&token)?;
    let commit = origin.peel(&token)?.ok_or_else(|| {
        Error::new(format!(
            "{label} is not an annotated tag peeled to a claim commit"
        ))
    })?;
    if !valid_lease_owner(&commit) {
        return Err(Error::new(format!(
            "{label} has no raw peeled claim object ID"
        )));
    }
    let commit = commit.to_ascii_lowercase();
    if token == commit {
        return Err(Error::new(format!(
            "{label} token equals its claim; refusing a lightweight/non-unique tag"
        )));
    }
    Ok(RemoteAnnotatedTag { token, commit })
}

fn remote_annotated_tag(origin: &dyn Origin, tag: &str) -> Result<Option<RemoteAnnotatedTag>> {
    remote_annotated_ref(
        origin,
        &format!("refs/tags/{tag}"),
        &format!("recovery tag {tag}"),
    )
}

/// Bind a published manifest's commit identity to the exact annotated git tag the release advertises.
pub fn assert_remote_annotated_tag_commit(
    origin: &dyn Origin,
    tag: &str,
    expected_commit: &str,
) -> Result<()> {
    let observed = remote_annotated_tag(origin, tag)?.ok_or_else(|| {
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

/// Delete an exact annotated tag token while an injected safety proof remains true.
pub fn delete_release_tag_with_guard(
    git: &dyn GitRunner,
    origin: &dyn Origin,
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
    let remote = remote_annotated_tag(origin, tag)?;
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
    before_each_delete()?;
    let written = origin.delete(&tag_ref, &remote.token).map_err(Error::new)?;
    if remote_annotated_tag(origin, tag)?.is_some() {
        return Err(Error::new(format!(
            "exact CAS delete of abandoned tag {tag} failed: {}",
            ref_write_description(written)
        )));
    }
    Ok(())
}

pub fn delete_owned_release_tag(
    git: &dyn GitRunner,
    origin: &dyn Origin,
    tag: &str,
    expected_commit: &str,
    lease_guard: &ReleaseLeaseGuard,
    fence_guard: &PublisherFenceGuard,
) -> Result<()> {
    delete_release_tag_with_guard(git, origin, tag, expected_commit, || {
        assert_publisher_session(origin, lease_guard, fence_guard)
    })
}

#[must_use]
pub const fn exact_delete_absence_is_converged(
    preexisting_absence_is_converged: bool,
    delete_attempted: bool,
) -> bool {
    preexisting_absence_is_converged || delete_attempted
}

fn github_delete(endpoint: &str) -> String {
    match crate::github::send_once(
        aterm_update_core::github_rest::Method::Delete,
        endpoint,
        None,
    ) {
        Ok(_) => String::new(),
        Err(error) => error.to_string(),
    }
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
        immediately_before_delete()?;
        let endpoint = format!("repos/{slug}/releases/{}", expected.id);
        let said = github_delete(&endpoint);
        delete_attempted = true;
        if release_object_by_id(slug, expected.id)?.is_none() {
            return Ok(true);
        }
        last = said;
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

/// What a cut that never made its release the head knows about the release it touched.
#[derive(Debug, Clone, Copy)]
pub struct WithdrawKnowledge<'a> {
    pub release_intent: Option<bool>,
    pub release_id: Option<u64>,
    pub upload_intents: Option<&'a [String]>,
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

/// Withdraw what an unpublished cut put on the channel, and nothing else.
pub fn withdraw_unpublished_release(
    repo: &Path,
    slug: &str,
    tag: &str,
    version: &str,
    known: WithdrawKnowledge<'_>,
    lease: &ReleaseLeaseGuard,
    fence: &PublisherFenceGuard,
) -> Result<String> {
    let origin = native_origin(repo)?;
    assert_publisher_session(&origin, lease, fence)?;
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
            || assert_publisher_session(&origin, lease, fence),
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
        assert_publisher_session(&origin, lease, fence)?;
        let endpoint = format!("repos/{slug}/releases/assets/{}", asset.id);
        let said = github_delete(&endpoint);
        if release_asset_identity_for_release_id_optional(slug, release.id, name)?
            .is_some_and(|(id, _)| id == asset.id)
        {
            return Err(Error::new(format!(
                "delete {name} (asset ID {}) from {tag} on {slug} failed: {said}",
                asset.id,
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

fn recovery_claim_build(
    git: &dyn GitRunner,
    origin: &dyn Origin,
    version: &str,
    owner: &str,
) -> Result<u64> {
    origin.import(owner).map_err(Error::new)?;
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

fn recovery_worktree_preflight(git: &dyn GitRunner, origin: &dyn Origin) -> Result<()> {
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
    let remote = origin.refresh_main().map_err(Error::new)?;
    let head = rev_parse(git, "HEAD")?;
    if head != remote {
        return Err(Error::new(format!(
            "lost-machine recovery requires HEAD == origin/main ({head} != {remote}); pull first"
        )));
    }
    Ok(())
}

/// Resume requires a clean tree, with no exceptions.
pub fn recovery_resume_worktree_preflight(
    _repo: &Path,
    git: &dyn GitRunner,
    _journal: &Journal,
) -> Result<()> {
    gates::clean_tree(git)
}

/// Bind an ordinary `--resume` to the immutable claim before the pipeline can reacquire either publication ref.
pub fn ordinary_resume_claim_preflight(
    repo: &Path,
    git: &dyn GitRunner,
    origin: &dyn Origin,
    journal: &Journal,
) -> Result<()> {
    recovery_resume_worktree_preflight(repo, git, journal)?;
    gates::current_own_cutter_gate(git)?;

    let remote_main = origin
        .refresh_main()
        .map_err(|error| Error::new(format!("cannot refresh origin/main for resume: {error}")))?;
    origin.import(&journal.commit).map_err(Error::new)?;
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
    let ancestor = git.git(&["merge-base", "--is-ancestor", &journal.commit, &remote_main])?;
    if !ancestor.success() {
        return Err(Error::new(format!(
            "journal claim {} is not an ancestor of origin/main; refusing a stale or foreign resume",
            journal.commit
        )));
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
        let mut values = text
            .lines()
            .filter_map(|line| line.strip_prefix(name)?.strip_prefix('='));
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
    origin: &dyn Origin,
    fence: &PublisherFenceGuard,
) -> Result<()> {
    let release = release_publisher_fence(origin, fence).map(|_| ());
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

/// Explicit cross-machine recovery for a persistent lease whose local journal was lost.
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
    let cargo_text = gates::read_bounded_string(&repo.join("Cargo.toml"), 16 << 20)
        .map_err(|error| Error::new(format!("read Cargo.toml: {error}")))?;
    let origin_slug = repo_slug(&cargo_text)
        .ok_or_else(|| Error::new("Cargo.toml repository is not an exact GitHub OWNER/REPO URL"))?;
    let slug = channel_slug(&cargo_text)?;
    let git = GitCli::new(repo);
    let origin = native_origin(repo)?;
    assert_origin_repo_binding(&git, &origin_slug)?;
    let journal_path = repo.join("dist/cut-state.toml");
    let header = JournalHeader::read(&journal_path)?;
    if let Some(header) = &header
        && (header.version != version || !header.commit.eq_ignore_ascii_case(&owner))
    {
        return Err(Error::new(format!(
            "local journal is v{} owner {}, not requested recovery v{version} {owner}",
            header.version, header.commit
        )));
    }
    if release_lease_owner(&origin)?.as_deref() != Some(owner.as_str()) {
        return Err(Error::new(format!(
            "persistent release lease is not owned by supplied claim {owner}; refusing recovery"
        )));
    }
    let build = recovery_claim_build(&git, &origin, version, &owner)?;
    if let Some(header) = &header
        && header.build_number != build
    {
        return Err(Error::new(format!(
            "local journal build {} differs from claim ledger tail {build}",
            header.build_number
        )));
    }
    let (tree, journal) = if header.is_some() {
        origin.refresh_main().map_err(Error::new)?;
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
        recovery_worktree_preflight(&git, &origin)?;
        (repo.to_path_buf(), None)
    };
    let remote_main = origin.refresh_main().map_err(Error::new)?;
    let ancestor = git.git(&["merge-base", "--is-ancestor", &owner, &remote_main])?;
    if !ancestor.success() {
        return Err(Error::new(format!(
            "recovery claim {owner} is not an ancestor of origin/main; refusing an unbound lease"
        )));
    }

    verify::release_state(&slug, &format!("v{version}"))?;
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
    if duty == RosterDuty::Sign
        && let Some(journal) = &journal
    {
        resume_attribution_agrees(
            journal.signature_machine_id.as_deref(),
            signature_verdict.machine_id().as_deref(),
        )?;
    }

    step("recover", RECOVERY_STOPPED_PROCESS_BANNER);
    let fence = rotate_publisher_fence_for_recovery(&git, &origin, &owner)?;
    let result = if let Some(journal) = journal {
        confirm_release_lease_owner(&origin, &owner).and_then(|lease| {
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
    combine_with_fence_release(result, &origin, &fence)
}

struct LostRecoveryPlan<'a> {
    version: &'a str,
    build: u64,
    owner: &'a str,
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
        operator_asserts_no_post,
    } = plan;
    let git = GitCli::new(repo);
    let origin = native_origin(repo)?;
    let lease = confirm_release_lease_owner(&origin, owner)?;
    assert_publisher_session(&origin, &lease, fence)?;
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
    withdraw_tag_then_unlock(&git, &origin, &tag, owner, &lease, fence)?;
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
    Ok(preflight_signature_policy(credentials, RosterDuty::Finish)?.policy)
}

fn recovered_roster_asset_names(manifest: &Manifest) -> Vec<&'static str> {
    if manifest.machine_id.is_none() {
        return Vec::new();
    }
    vec![roster::ROSTER_ASSET, roster::ROSTER_SIG_ASSET]
}

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
    let parsed = machines::verify_published_roster_parsed(
        aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        roster_bytes,
        &roster_sig,
        machine_id,
        manifest.roster_seq,
    )?;
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
        gates::read_bounded_bytes(&local, 16 << 20),
        gates::read_bounded_bytes(&dist.join(roster::ROSTER_SIG_ASSET), 16 << 20),
    ) else {
        return Ok(());
    };
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
    let incoming_seq = machines::verify_published_roster(
        aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        roster_bytes.clone(),
        &roster_sig,
        machine_id,
        manifest.roster_seq,
    )?;
    refuse_roster_downgrade(dist, incoming_seq)?;
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
    let origin = native_origin(repo)?;
    assert_publisher_session(&origin, &lease, &fence)?;
    let tag = format!("v{version}");
    let remote_tag = remote_annotated_tag(&origin, &tag)?.ok_or_else(|| {
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
    if !buildplan::windows::manifest_asset_names(&manifest)?.is_empty() {
        return Err(Error::new(
            "published Windows recovery requires the original cut journal and frozen Windows handoff; do not reconstruct its build's provenance from remote filenames",
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
    crate::durable::copy_regular_fresh(&recovered_dmg, dist.join(channel::stable_dmg_asset_name()))
        .map_err(|error| Error::new(format!("reconstruct stable dmg twin: {error}")))?;
    verify_release_asset_digest_for_release_id_to(
        slug,
        release_object.id,
        &tag,
        &recovered_zip,
        &recovered_zip_sha256,
        &dist.join(&recovered_zip),
    )?;
    crate::durable::copy_regular_fresh(
        dist.join(&recovered_zip),
        dist.join(channel::stable_zip_asset_name()),
    )
    .map_err(|error| Error::new(format!("reconstruct stable zip twin: {error}")))?;
    refuse_retired_intel_dmg(&manifest)?;
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
        crate::durable::write_fresh(
            &dist.join(&sidecar),
            channel::sha256_sidecar_contents(sha, name),
        )
        .map_err(|error| Error::new(format!("reconstruct {sidecar}: {error}")))?;
    }
    crate::durable::write_fresh(&dist.join(manifest_out::MANIFEST_ASSET), &manifest_bytes)
        .map_err(|error| Error::new(format!("reconstruct manifest: {error}")))?;
    if let Some(signature) = &signature_bytes {
        crate::durable::write_fresh(&dist.join(manifest_out::MANIFEST_SIG_ASSET), signature)
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
    let qualification_digest = crate::service_qualification::report_digest(&manifest_bytes)?;
    let report_name = crate::service_qualification::REPORT_ASSET;
    if !exact_asset_present(&names, report_name)? {
        return Err(Error::new(
            "published recovery release has no service qualification report",
        ));
    }
    verify_release_asset_digest_for_release_id_to(
        slug,
        release_object.id,
        &tag,
        report_name,
        &qualification_digest,
        &dist.join(report_name),
    )?;
    crate::service_qualification::verify_published_report(
        &dist.join(manifest_out::MANIFEST_ASSET),
    )?;
    let journal_path = dist.join("cut-state.toml");
    let journal = Journal {
        format: JOURNAL_FORMAT,
        version: version.to_string(),
        build_number: build,
        commit: owner.to_string(),
        min_build: manifest.min_build,
        artifact: None,
        macho_sha256: None,
        linux: None,
        windows: None,
        manifest_signed: signature_policy.required,
        signature_required: signature_policy.required,
        signature_pubkey: signature_policy.pubkey.clone(),
        verify_pubkey: recovered_pubkey.clone(),
        signature_machine_id: manifest.machine_id.clone(),
        release_id: Some(release_object.id),
        release_intent: true,
        upload_intents: Vec::new(),
        delete_intent: None,
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
        apple: sign::AppleTier::Inactive,
        repo: repo.to_path_buf(),
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
        windows: None,
        signature_required: signature_policy.required,
        signature_pubkey: signature_policy.pubkey.clone(),
        verify_pubkey: recovered_pubkey.clone(),
        signature_machine_id: manifest.machine_id.clone(),
        attribution: None,
        roster: None,
        release_id: Some(release_object.id),
        release_intent: true,
        upload_intents: Vec::new(),
        kind: CutKind::Real,
        lease: Some(lease),
        fence: Some(fence),
        journal: Some(journal),
        adopted: false,
    };
    run_pipeline(&mut ctx, Instant::now())
}

/// The notes a cut ships: the release commit `commit`'s own `## [version]` section
/// ([`changelog::rolled_body`]), read from its objects in every mode, never from a working tree.
/// A dry run's release commit is rolled exactly as a real one's and a recut's carries its
/// section as it stands, so every mode ships the same bytes.
///
/// # Errors
/// The commit's changelog cannot be read, is not UTF-8, or has no such section.
pub fn release_body_of(git: &dyn ledger::GitRunner, commit: &str, version: &str) -> Result<String> {
    let spec = format!("{commit}:{}", changelog::CHANGELOG_FILE);
    let out = ledger::git_read_ok(
        git,
        &[
            "--no-lazy-fetch",
            "--no-replace-objects",
            "cat-file",
            "blob",
            &spec,
        ],
    )
    .map_err(|e| Error::new(format!("read {spec}: {e}")))?;
    let text = String::from_utf8(out.stdout)
        .map_err(|_| Error::new(format!("{spec} is not valid UTF-8")))?;
    changelog::rolled_body(&text, version)
}

/// The transcript's `signature` line.
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

/// What a REAL cut publishes, and whether its claim is a recut.
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

/// A dry run's statement of the commit a REAL cut would release.
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

/// What a fresh cut (not `--resume`) does with a journal already on disk, of any format, read by its [`JournalHeader`].
pub fn fresh_cut_journal_triage(existing: Option<&JournalHeader>, kind: CutKind) -> Result<bool> {
    let Some(j) = existing else {
        return Ok(false);
    };
    if j.finished() {
        return Ok(true);
    }
    let at = j.position();
    match kind {
        CutKind::Real | CutKind::Draft => Err(Error::new(format!(
            "a cut is already in progress: v{} (build {}) is journaled {at} — finish it with \
             `{CUT_COMMAND}`, discard it with `{CUT_COMMAND} --abandon v{}`, or \
             delete dist/cut-state.toml",
            j.version, j.build_number, j.version
        ))),
        CutKind::DryRun => Err(Error::new(format!(
            "an unfinished real cut is journaled: v{} (build {}) {at} — a dry-run would \
                 overwrite its dist/ artifacts with provisional-number ones; finish it \
                 (`{CUT_COMMAND}`) or discard it (`{CUT_COMMAND} --abandon v{}`) first",
            j.version, j.build_number, j.version
        ))),
    }
}

/// Put the cut tree at the claim commit `header` names.
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

/// The bundle a real cut signs.
pub struct VerifiedBundle {
    pub reserved: ledger::Reserved,
    pub complete: artifact::Complete,
    pub line: String,
}

/// Read the verified release bundle of `source` from this clone's release journal.
pub fn verified_release_bundle(
    operator: &dyn GitRunner,
    repo: &Path,
    source: &str,
    version: &str,
    toolchain: &str,
) -> Result<VerifiedBundle> {
    verified_release_bundle_with_inputs(
        operator,
        repo,
        source,
        version,
        toolchain,
        crate::build_inputs::current,
    )
}

pub(crate) fn verified_release_bundle_with_inputs(
    operator: &dyn GitRunner,
    repo: &Path,
    source: &str,
    version: &str,
    toolchain: &str,
    inputs: impl FnOnce() -> std::result::Result<String, String>,
) -> Result<VerifiedBundle> {
    release_bundle_with_inputs(operator, repo, source, version, toolchain, inputs, true)
}

pub(crate) fn release_bundle_with_inputs(
    operator: &dyn GitRunner,
    repo: &Path,
    source: &str,
    version: &str,
    toolchain: &str,
    inputs: impl FnOnce() -> std::result::Result<String, String>,
    require_verified: bool,
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
    let line = journal.line();
    if line.head() != source {
        return Err(refuse(format!(
            "this clone's release journal names {} and its line's head {}: they must be one \
             commit",
            short(source),
            short(line.head())
        )));
    }
    if !line.hops.is_empty()
        && let Err(stop) = crate::line::verify(operator, &line)
    {
        return Err(refuse(format!(
            "the line's meaning check (line::verify) refuses its head {}: {stop}",
            short(source)
        )));
    }
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
        .map_err(refuse)?
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
    let inputs = inputs().map_err(Error::new)?;
    if complete.inputs != inputs {
        return Err(refuse(
            "the compiler or SDK bytes changed since this bundle was built".into(),
        ));
    }
    let line = if require_verified {
        let (yes, line) =
            gates::verified_with_inputs(operator, source, &complete.macho_sha256, || Ok(inputs))?;
        if !yes {
            return Err(refuse(format!("NOTHING SHIPS UNVERIFIED: {line}")));
        }
        line
    } else {
        "built bytes re-proved; private preparation only, verification still required".into()
    };
    Ok(VerifiedBundle {
        reserved: reserved.ledger(),
        complete,
        line,
    })
}

/// Run `targo --unverified ship cut`.
pub fn run_cut(repo: &Path, opts: &CutOptions) -> Result<()> {
    let _timing = crate::cut_timing::Timer::new("cut total");
    let preflight = crate::cut_timing::Timer::new("cut preflight");
    gates::arm_toolchain_lease();
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

    let existing = JournalHeader::read(&journal_path)?;
    if opts.resume {
        let Some(header) = existing else {
            let lock = match native_origin(repo).and_then(|origin| release_lease_owner(&origin)) {
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
        let tree = place_claim_tree(&operator, repo, &header)?;
        gates::own_cutter_gate(&GitCli::new(&tree), &header.commit, false)?;
        let j = Journal::load(&journal_path)?.ok_or_else(|| {
            Error::new(format!(
                "{} vanished while resuming",
                journal_path.display()
            ))
        })?;
        let slug = workspace_channel_slug(&tree)?;
        drop(preflight);
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
        let _ = fs::remove_file(&journal_path);
    }
    let published = if kind == CutKind::Real {
        let source =
            crate::cut_timing::measure("published source", || gates::published_source(repo))?;
        let origin = native_origin(repo)?;
        let checkout = crate::cut_timing::measure("place published source", || {
            gates::place_published(&operator, &origin, repo, &source)
        })?;
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
        step(
            "source",
            &dry_run_source_line(&gates::published_source(repo)),
        );
        None
    };
    let tree = published
        .as_ref()
        .map_or_else(|| repo.to_path_buf(), |checkout| checkout.tree.clone());
    let git = GitCli::new(&tree);
    let cargo_text = gates::read_bounded_string(&tree.join("Cargo.toml"), 16 << 20)
        .map_err(|e| Error::new(format!("read {}: {e}", tree.join("Cargo.toml").display())))?;
    let full = workspace_version(&cargo_text)?;
    let origin_slug = repo_slug(&cargo_text).ok_or_else(|| {
        Error::new(
            "Cargo.toml [workspace.package] repository is not an exact GitHub OWNER/REPO URL",
        )
    })?;
    if kind == CutKind::Real {
        assert_origin_repo_binding(&git, &origin_slug)?;
    }
    let publish_slug = channel_slug(&cargo_text)?;
    let release_version = release_version_from_workspace(&full)?;
    set_publisher_fence_version(&release_version);

    let linux = opts
        .linux_artifacts
        .as_deref()
        .map(|directory| {
            buildplan::linux::Handoff::new(directory, &opts.linux_targets, &opts.linux_workers)
        })
        .transpose()?;
    let windows = opts
        .windows_artifacts
        .as_deref()
        .map(|dir| buildplan::windows::Handoff::new(dir, opts.windows_sha256.as_deref()))
        .transpose()?;
    let apple = resolve_apple_tier(aterm_update_core::pins::APPLE_TEAM_ID, credentials)?;

    let changelog_text =
        gates::read_bounded_string(&tree.join(changelog::CHANGELOG_FILE), 64 << 20)
            .map_err(|e| Error::new(format!("read {}: {e}", changelog::CHANGELOG_FILE)))?;
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
        (release_version.clone(), false)
    };
    ledger::check_version_shape(&version)?;

    let head8: String = rev_parse(&git, "HEAD")?.chars().take(8).collect();
    let flavor = match kind {
        CutKind::Real if recut => " [recut]",
        CutKind::Real => "",
        CutKind::DryRun => " [dry-run]",
        CutKind::Draft => unreachable!("drafts are prepared by cut --sign"),
    };
    println!("aterm-release · cut v{version} ({head8}){flavor}");

    let gate_opts = gates::GateOpts {
        version: version.clone(),
        offline: !matches!(kind, CutKind::Real),
        allow_stale_cutter: matches!(kind, CutKind::DryRun),
        published: published.clone(),
    };
    let gate_origin = if gate_opts.offline {
        None
    } else {
        Some(native_origin(&tree)?)
    };
    let gr = gates::run_all(
        &git,
        gate_origin
            .as_ref()
            .map(|origin| origin as &dyn ledger::Origin),
        &tree,
        repo,
        &gate_opts,
    )?;
    let tag = if gate_opts.offline {
        "no tag claimed (dry-run)".to_string()
    } else {
        format!("tag v{version} free (local+remote)")
    };
    step(
        "gates",
        &format!(
            "clean tree · HEAD {} ({}) · {tag}",
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
            "GitHub as {} · disk ok ({} GiB free)",
            gr.github_login
                .as_deref()
                .unwrap_or("no one (this run publishes nothing)"),
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
    let toolchain = gates::cut_toolchain()
        .map_err(|e| Error::new(format!("the cut cannot name its prover: {e}")))?;
    let source = match &published {
        Some(checkout) => checkout.source.commit.clone(),
        None => rev_parse(&git, "HEAD")?,
    };
    // COMMIT-BEFORE-CLAIM: these read the fixed source and journal, not one
    // another's outputs. Join every verdict before claiming the reserved build.
    // The bundle reader requires this source's COMMIT_GATES fact and proves
    // its bytes, compiler and inputs seal. Re-running its immutable fixture and
    // policy judgments here would repeat qualify's work with fresh tag imports.
    let commit_proofs = || {
        crate::cut_timing::measure("verified release bundle", || {
            let verified = verified_release_bundle(&operator, repo, &source, &version, &toolchain)?;
            gates::CommitGateRecord::new(
                &source,
                &version,
                verified.complete.inputs.clone(),
                native_origin(&tree)?
                    .tag_fingerprint()
                    .map_err(Error::new)?,
            )
            .adopt(&operator)?;
            Ok::<_, Error>(verified)
        })
    };
    let availability = || {
        crate::cut_timing::independent(
            ("release lock preflight", || {
                if kind == CutKind::Real {
                    preflight_release_lease(&native_origin(repo)?)
                } else {
                    Ok(())
                }
            }),
            ("channel target preflight", || {
                if kind == CutKind::Real {
                    preflight_channel_target(&publish_slug, false)
                } else {
                    Ok(())
                }
            }),
        )
    };
    let head_and_signature = || {
        crate::cut_timing::independent(
            ("channel head and floor", || {
                verify::scan_head_before_claim(&publish_slug)
            }),
            ("signature policy", || {
                preflight_signature_policy(credentials, RosterDuty::Sign)
            }),
        )
    };
    let roster_and_ledger = || {
        crate::cut_timing::independent(
            ("channel roster", || {
                if kind == CutKind::Real {
                    machines::channel_roster_document(&publish_slug)
                } else {
                    Ok(None)
                }
            }),
            ("ledger floor", || {
                if kind == CutKind::Real {
                    ledger::show_origin_ledger(&git)
                } else {
                    gates::read_bounded_string(&tree.join(ledger::LEDGER_FILE), 64 << 20)
                        .map_err(|e| Error::new(format!("read {}: {e}", ledger::LEDGER_FILE)))
                }
            }),
        )
    };
    // All leaves read existing state. Only the reconciliation below can
    // authorize a claim; no preflight writer or claim races these readers.
    let (commit, (availability, floors)) = crate::cut_timing::independent(
        ("commit proofs", commit_proofs),
        ("claim authority", || {
            crate::cut_timing::independent(
                ("availability", availability),
                ("public floors", || {
                    crate::cut_timing::independent(
                        ("head and signature", head_and_signature),
                        ("roster and ledger", roster_and_ledger),
                    )
                }),
            )
        }),
    );
    let verified = commit;
    let (lock, channel) = availability;
    let ((head_scan, signature_verdict), (observed_roster, ledger_text)) = floors;
    let verified = verified?;
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
    step("gate", &verified.line);
    if kind != CutKind::Real && !opts.linux_workers.is_empty() {
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
        windows_bound_before_claim(windows.as_ref(), &dist)?;
    }
    if kind == CutKind::Real {
        lock?;
        channel?;
        step("lock", "release lock is free");
        step(
            "channel",
            &format!("release channel {publish_slug} is public and writable (pre-claim)"),
        );
    }

    let head_scan = head_scan?;
    let newest_channel = head_scan.first();
    let newest_min_build = newest_channel.and_then(|published| published.min_build);
    let signature_verdict =
        signature_verdict.map_err(|e| Error::new(format!("{}; nothing was claimed", e.0)))?;
    let signature_policy = signature_verdict.policy.clone();
    step(
        "signature",
        &signature_transcript_line(
            signature_policy.pubkey.as_deref(),
            signature_verdict.machine_id().as_deref(),
        ),
    );
    let manifest_roster_seq = published_roster_seq(newest_channel)?;
    let observed_roster = observed_roster.map_err(|e| {
        Error::new(format!(
            "cannot read the machine roster on the channel {publish_slug}'s latest \
                 release ({e}); refusing to reason about the fleet's roster floor — a wrong \
                 answer here burns a build number and strands every client"
        ))
    })?;
    let observed_roster_seq = observed_roster.as_ref().map(|(seq, _)| *seq);
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

    let now = unix_now();
    let ledger_text = ledger_text?;
    let tail = ledger::tail(&ledger_text)?;
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

    drop(preflight);
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
            let origin = native_origin(&tree)?;
            let base = crate::gates::harness_versions_base(&git, &origin, &source, &version)?;
            let plan = ledger::ClaimPlan {
                version: &version,
                now,
                allow_existing_section: recut,
                max_attempts: ledger::MAX_CLAIM_ATTEMPTS,
                source: &source,
                harness_base: base.as_ref().map(|(r, c)| (r.as_str(), c.as_str())),
            };
            let claim = crate::cut_timing::measure("claim and verify main", || {
                ledger::claim(&git, &origin, &tree, &plan, &verified.reserved)
            })
            .map_err(|error| discard_on_lost_race(&git, &dist, &version, reserved, error))?;
            step(
                "",
                &format!(
                    "pushed \"release: v{version} (build {})\" — the release commit {} is the \
                     published {} plus the ledger line and the rolled changelog, less the entry \
                     files it shipped; main took it {}  [verified: origin/main == {}, ledger tail == \"{}\"]",
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
            if !claim.harness.is_empty() {
                // Said once, where the merge is: the next landing owes none of these.
                let names: Vec<String> = claim
                    .harness
                    .iter()
                    .map(|o| format!("alab {} harness {}.{}", o.harness.cli(), o.to.0, o.to.1))
                    .collect();
                step(
                    "",
                    &format!(
                        "the merge raised the harness versions main owes the window v{version} \
                         opens ({}): their sources changed on main past the release commit",
                        names.join(", ")
                    ),
                );
            }
            (claim.build, claim.commit)
        }
        CutKind::Draft => unreachable!("drafts never claim"),
        CutKind::DryRun => {
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
    let journal_timing = crate::cut_timing::Timer::new("prepare cut journal");
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
        version,
        build,
        commit,
        min_build,
        artifact: Some(verified.complete.key.clone()),
        macho_sha256: Some(verified.complete.macho_sha256.clone()),
        linux,
        windows,
        manifest_signed: false,
        signature_required: signature_policy.required,
        signature_pubkey: signature_policy.pubkey,
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
        adopted: false,
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
            windows: ctx.windows.clone(),
            manifest_signed: ctx.manifest_signed,
            signature_required: ctx.signature_required,
            signature_pubkey: ctx.signature_pubkey.clone(),
            verify_pubkey: ctx.verify_pubkey.clone(),
            signature_machine_id: ctx.signature_machine_id.clone(),
            release_id: None,
            release_intent: false,
            upload_intents: Vec::new(),
            delete_intent: None,
            done: vec![],
        };
        j.save(&journal_path)?;
        ctx.journal = Some(j);
        draft::adopt(&mut ctx)?;
    }

    drop(journal_timing);
    run_pipeline(&mut ctx, t0)
}

/// A real cut that will admit a Windows zip itself (no private draft signed one first) is bound
/// to the digest the person named before it claims: the admission comes after the claim, and
/// `cut --resume` takes no flag, so a cut that claimed without one could never name it.
fn windows_bound_before_claim(
    windows: Option<&buildplan::windows::Handoff>,
    dist: &Path,
) -> Result<()> {
    let Some(handoff) = windows else {
        return Ok(());
    };
    if handoff.sha256.is_some() || dist.join(draft::JOURNAL).exists() {
        return Ok(());
    }
    Err(Error::new(format!(
        "--windows-artifacts {} names no --windows-sha256: a carried Windows pair is admitted \
         only by the zip's SHA-256 as release-build printed it on the Windows host, and this cut \
         would admit it after its claim, where `--resume` takes no flag; nothing was claimed. \
         Build the pair with `aterm-winship release-build` at the reserved commit on a Windows \
         host (docs/RELEASING.md § Windows release handoff) and give its digest, or release \
         with `{CUT_COMMAND}`, whose `cut --sign` admits it before any claim",
        handoff.directory.display()
    )))
}

/// Whether origin's ledger `ledger_text` was claimed at or past `build` (by another release):
/// the race [`ledger::claim`] refuses, after which build `build` can never be published.
pub(crate) fn lost_the_race(ledger_text: &str, build: u64) -> bool {
    ledger::tail(ledger_text).is_ok_and(|tail| tail.build >= build)
}

/// A claim's `error`, and when it lost the ledger's race ([`lost_the_race`]) the discard of the
/// copy `cut --sign` signed of build `build`: that number is spent elsewhere, so the copy is
/// never adopted and never made public.
fn discard_on_lost_race(
    git: &dyn GitRunner,
    dist: &Path,
    version: &str,
    build: u64,
    error: Error,
) -> Error {
    let lost = ledger::show_origin_ledger(git).is_ok_and(|text| lost_the_race(&text, build));
    if !lost {
        return error;
    }
    let fate = discard_signed(
        dist,
        version,
        build,
        crate::release::SignedDiscard::LostReservation,
    )
    .fate();
    Error::new(format!(
        "{error}. The copy signed, notarized and packaged privately as build {build} {fate}; \
         nothing of it was made public"
    ))
}

/// Whether `cut --sign` may sign beside the cut journaled at `path`: never beside an unfinished
/// one, which signs its own bundle as it finishes; a finished one is an earlier release's, which
/// the next fresh cut clears ([`fresh_cut_journal_triage`]).
fn sign_beside_journal(existing: Option<&JournalHeader>, path: &Path) -> Result<()> {
    match existing {
        Some(header) if !header.finished() => Err(Error::new(format!(
            "a claimed cut of v{} (build {}) is journaled in this checkout ({}): it signs \
             its own bundle as it finishes (`{CUT_COMMAND} --resume`), and nothing is signed \
             beside it. Nothing was signed",
            header.version,
            header.build_number,
            path.display()
        ))),
        _ => Ok(()),
    }
}

/// `cut --sign`: copy this clone's verified release bundle byte for byte, sign it, notarize and
/// staple it, package and notarize its DMG, zip it, and run the self-check's local half on it,
/// all under the reserved number and commit and all private, then keep the record the claim
/// adopts ([`SignedRecord`], [`adopted_copy`]). It reads its release from this clone's release
/// journal (R, the reservation, the bundle), never from the publication engine, so it runs
/// beside `pub publish`: it claims nothing and takes no release lock or publisher fence.
/// After signing, it stages the manifest and uploads every asset into a journaled private
/// draft; the engine attaches its source attestation to that exact draft without publishing
/// it. Linux workers receive the reserved commit as a private bundle, and every declared
/// handoff is imported before its manifest is signed. A standing record of
/// these bytes is adopted as it is. `--sign-prepare` uses the built bundle before verification
/// ends, executes no product, and writes only a prepared record. After verification `--sign`
/// re-proves it, runs the local self-check and writes the record the claim may adopt.
pub fn run_cut_sign(repo: &Path, opts: &CutOptions) -> Result<()> {
    gates::arm_toolchain_lease();
    let credentials = sign::ReleaseCredentials::resolve(opts.release_credentials.as_deref(), repo)
        .map_err(Error::new)?;
    let credentials = credentials.as_ref();
    let t0 = Instant::now();
    let dist = repo.join("dist");
    let journal_path = dist.join("cut-state.toml");
    let operator = GitCli::new(repo);
    assert_origin_repo_binding(&operator, &workspace_repo_slug(repo)?)?;
    sign_beside_journal(JournalHeader::read(&journal_path)?.as_ref(), &journal_path)?;
    let release = crate::release::Journal::load(
        &crate::release::release_dir(&operator)
            .map_err(Error::new)?
            .join(crate::release::JOURNAL_FILE),
    )
    .map_err(Error::new)?
    .ok_or_else(|| {
        Error::new(format!(
            "this clone has no release journal: `cut --sign` signs the bundle its release \
             built and verified (`{}`). Nothing was signed",
            crate::release::RELEASE_COMMAND
        ))
    })?;
    let (source, version) = (release.commit.clone(), release.version.clone());
    ledger::check_version_shape(&version)?;
    let tree = gates::place_cut_tree(&operator, repo, &source)?.path;
    let git = GitCli::new(&tree);
    gates::own_cutter_gate(&git, &source, false)?;
    let slug = workspace_channel_slug(&tree)?;
    println!(
        "aterm-release · cut --sign v{version} ({}) — private: nothing is claimed or published",
        &source[..8.min(source.len())]
    );
    let toolchain = gates::cut_toolchain()
        .map_err(|e| Error::new(format!("the cut cannot name its prover: {e}")))?;
    let verified = release_bundle_with_inputs(
        &operator,
        repo,
        &source,
        &version,
        &toolchain,
        crate::build_inputs::current,
        !opts.sign_prepare,
    )?;
    step("gate", &verified.line);
    let parent = rev_parse(&git, &format!("{}^1", verified.reserved.commit))?;
    if parent != source {
        return Err(Error::new(format!(
            "the reserved release commit {} is built on {parent}, not on R {source}: it is \
             never signed. Nothing was signed",
            verified.reserved.commit
        )));
    }
    let origin = native_origin(&tree)?;
    let fixtures = gates::handoff_fixture_gate_at(&git, &origin, &source, &version)?;
    step("gate", &gates::handoff_fixtures_line(fixtures.as_ref()));
    let policy = gates::handoff_policy_gate_at(&git, &origin, &source, &version)?;
    step(
        "gate",
        &format!("handoff policy sealed into the bundle: {policy}"),
    );
    let apple = resolve_apple_tier(aterm_update_core::pins::APPLE_TEAM_ID, credentials)?;
    let signature_verdict = preflight_signature_policy(credentials, RosterDuty::Sign)
        .map_err(|e| Error::new(format!("{}; nothing was signed", e.0)))?;
    let signature_policy = signature_verdict.policy.clone();
    step(
        "signature",
        &signature_transcript_line(
            signature_policy.pubkey.as_deref(),
            signature_verdict.machine_id().as_deref(),
        ),
    );
    // A dry run's context: no ledger claim, no journal, no release lock and no publisher fence;
    // the claim that adopts this copy holds all four.
    let attributed = signature_verdict.cut_attribution();
    let mut ctx = CutCtx {
        credentials: credentials.cloned(),
        apple,
        repo: repo.to_path_buf(),
        tree,
        dist,
        journal_path,
        slug,
        tag: format!("v{version}"),
        version,
        build: verified.reserved.build,
        commit: verified.reserved.commit.clone(),
        min_build: opts.min_build,
        artifact: Some(verified.complete.key.clone()),
        macho_sha256: Some(verified.complete.macho_sha256.clone()),
        linux: None,
        windows: None,
        manifest_signed: false,
        signature_required: signature_policy.required,
        signature_pubkey: signature_policy.pubkey,
        verify_pubkey: None,
        signature_machine_id: attributed.machine_id,
        attribution: attributed.attribution,
        roster: attributed.roster,
        release_id: None,
        release_intent: false,
        upload_intents: Vec::new(),
        kind: CutKind::DryRun,
        lease: None,
        fence: None,
        journal: None,
        adopted: false,
    };
    let source = signing_source_with_verification(&ctx, !opts.sign_prepare)?;
    if adopted_copy(&ctx, &source)?.is_none() {
        let signed = match prepared_copy(&ctx, &source)? {
            Some(signed) => signed,
            None => sign_copy(&ctx, &source)?,
        };
        prove_bundle_on_disk(&ctx)?;
        let record = SignedRecord {
            format: SIGNED_FORMAT,
            key: signed_key(&ctx, &source.complete)?,
            outputs: signed_outputs(&ctx.dist, &ctx.version, ctx.build).map_err(Error::new)?,
        };
        outputs_as_signed(&record.outputs, &signed)?;
        finish_private_copy(&ctx.dist, &record, opts.sign_prepare, || {
            let apple_note = selfcheck_local(&ctx, aterm_update_core::pins::APPLE_TEAM_ID)?;
            step(
                "selfcheck",
                &format!(
                    "binary == plist == {} · codesign --verify --deep --strict ok{apple_note}",
                    ctx.build
                ),
            );
            Ok(())
        })?;
    }
    if !opts.sign_prepare {
        let mut declaration = opts.clone();
        if !declaration.mac_only && declaration.linux_artifacts.is_none() {
            declaration.mac_only = release.linux.mac_only;
            declaration.linux_artifacts = release.linux.artifacts;
            declaration.linux_targets = release.linux.targets;
            declaration.linux_workers = release.linux.workers;
        }
        if declaration.windows_artifacts.is_none() {
            declaration.windows_artifacts = release.windows;
        }
        if declaration.windows_sha256.is_none() {
            declaration.windows_sha256 = release.windows_sha256;
        }
        draft::prepare(&mut ctx, &declaration)?;
    }
    println!(
        "\nDONE cut {} v{} (build {}) in {}s — private record ({})",
        if opts.sign_prepare {
            "--sign-prepare"
        } else {
            "--sign"
        },
        ctx.version,
        ctx.build,
        t0.elapsed().as_secs(),
        if opts.sign_prepare {
            prepared_record_path(&ctx.dist, ctx.build)
        } else {
            signed_record_path(&ctx.dist, ctx.build)
        }
        .display()
    );
    Ok(())
}

/// Only the checked phase may create the record the claim adopts. The reproof after the
/// callback also refuses a self-check that changed any output it executed or inspected.
fn finish_private_copy(
    dist: &Path,
    record: &SignedRecord,
    prepare: bool,
    selfcheck: impl FnOnce() -> Result<()>,
) -> Result<()> {
    signed_copy_stands(record, &record.key, dist).map_err(Error::new)?;
    if prepare {
        return write_copy_record(&prepared_record_path(dist, record.key.build), record);
    }
    selfcheck()?;
    signed_copy_stands(record, &record.key, dist).map_err(Error::new)?;
    write_signed_record(dist, record)?;
    forget_copy_record(&prepared_record_path(dist, record.key.build))
}

struct ResumePaths<'a> {
    repo: &'a Path,
    tree: &'a Path,
    dist: &'a Path,
    journal_path: &'a Path,
}

fn resume_cut(
    paths: ResumePaths<'_>,
    slug: &str,
    journal: Journal,
    t0: Instant,
    recovered_session: Option<(ReleaseLeaseGuard, PublisherFenceGuard)>,
    credentials: Option<&sign::ReleaseCredentials>,
) -> Result<()> {
    let resume_timing = crate::cut_timing::Timer::new("resume preflight");
    let ResumePaths {
        repo,
        tree,
        dist,
        journal_path,
    } = paths;
    let Some(next) = journal.first_incomplete() else {
        return Err(Error::new(format!(
            "the journaled cut already completed every step — nothing to resume \
             (delete dist/cut-state.toml, or just cut: a fresh cut clears it). The website \
             follows the release in its after-cut step: `{}`",
            crate::release::RELEASE_COMMAND
        )));
    };
    let git = GitCli::new(tree);
    let origin = native_origin(tree)?;
    set_publisher_fence_version(&journal.version);
    println!(
        "aterm-release · cut v{} (build {}) — RESUME at step \"{next}\"",
        journal.version, journal.build_number
    );

    ordinary_resume_claim_preflight(tree, &git, &origin, &journal)?;

    if recovered_session.is_none() && next != "unlock" {
        match reclaim_dead_publisher_fence(&origin, &journal.commit, &LocalProbe) {
            Ok(FenceReclaim::NoFence) => {}
            Ok(FenceReclaim::Reclaimed { token, detail }) => {
                println!("  fence: reclaimed {PUBLISHER_FENCE_REF} (token {token}) — {detail}")
            }
            Ok(FenceReclaim::Kept { refusal }) => {
                println!("  fence: held by another publisher session\n{refusal}");
            }
            Err(error) => println!("  fence: liveness probe could not run: {error}"),
        }
    }

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
        build: journal.build_number,
        commit: journal.commit.clone(),
        min_build: journal.min_build,
        artifact: journal.artifact.clone(),
        macho_sha256: journal.macho_sha256.clone(),
        linux: journal.linux.clone(),
        windows: journal.windows.clone(),
        manifest_signed: journal.manifest_signed,
        signature_required: journal.signature_required,
        signature_pubkey: journal.signature_pubkey.clone(),
        verify_pubkey: journal.verify_pubkey.clone(),
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
        adopted: false,
    };
    drop(resume_timing);
    run_pipeline(&mut ctx, t0)
}

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

fn run_pipeline(ctx: &mut CutCtx, t0: Instant) -> Result<()> {
    let prepare = crate::cut_timing::Timer::new("pipeline prepare");
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
            &gates::read_bounded_string(&ctx.manifest_path(), 16 << 20)
                .map_err(|e| Error::new(format!("read resumed appcast: {e}")))?,
        )?;
        buildplan::linux::verify_manifest_handoff(&manifest, ctx.linux.as_ref())?;
    }
    if let Some(handoff) = &ctx.windows
        && ctx
            .journal
            .as_ref()
            .is_some_and(|journal| journal.is_done("sign"))
    {
        handoff.verify_staged(&ctx.dist, &ctx.version, ctx.build, &ctx.commit)?;
        let manifest = Manifest::parse(
            &gates::read_bounded_string(&ctx.manifest_path(), 16 << 20)
                .map_err(|e| Error::new(format!("read resumed appcast: {e}")))?,
        )?;
        buildplan::windows::verify_manifest_handoff(&manifest, Some(handoff))?;
    }
    if ctx.kind == CutKind::Real
        && !matches!(
            ctx.journal.as_ref().and_then(Journal::first_incomplete),
            Some("unlock")
        )
    {
        if ctx.lease.is_none() {
            let origin = native_origin(&ctx.repo)?;
            ctx.lease = Some(crate::cut_timing::measure("acquire release lease", || {
                acquire_release_lease(&origin, &ctx.commit)
            })?);
        }
        if ctx.fence.is_none() {
            let git = GitCli::new(&ctx.repo);
            let origin = native_origin(&ctx.repo)?;
            ctx.fence = Some(crate::cut_timing::measure(
                "acquire publisher fence",
                || acquire_publisher_fence(&git, &origin, &ctx.commit),
            )?);
        }
        crate::cut_timing::measure("revalidate signature policy", || {
            revalidate_ctx_signature_policy(ctx)
        })?;
    }
    drop(prepare);
    let result = run_pipeline_inner(ctx, t0);
    let _cleanup = crate::cut_timing::Timer::new("pipeline cleanup");
    let fence_release = if let Some(fence) = ctx.fence.take() {
        native_origin(&ctx.repo)
            .and_then(|origin| release_publisher_fence(&origin, &fence))
            .map(|_| ())
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
        let _timing = crate::cut_timing::Timer::new(name);
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
        crate::cut_timing::measure("journal completed step", || ctx.mark(name))?;
    }

    match ctx.kind {
        CutKind::Real => {
            let _tidy = crate::cut_timing::Timer::new("staging cleanup");
            discard_staged(ctx);
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
        CutKind::Draft => unreachable!("draft preparation never runs the claimed pipeline"),
    }
    Ok(())
}

/// A real cut's closing transcript line.
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

fn ensure_ctx_release_lease(ctx: &CutCtx) -> Result<()> {
    release_session(ctx)?.check()
}

/// An owned snapshot lets upload workers check remote authority without holding
/// the journal mutex across network reads. Tokens remain freshly observed for
/// every mutation; only durable journal writes need serialization.
struct ReleaseSession {
    repo: PathBuf,
    guards: Option<(ReleaseLeaseGuard, PublisherFenceGuard)>,
    reservation: Option<(String, u64)>,
}

impl ReleaseSession {
    fn check(&self) -> Result<()> {
        if let Some((commit, build)) = &self.reservation {
            return draft::authority(&self.repo, commit, *build);
        }
        let Some((lease, fence)) = &self.guards else {
            return Ok(());
        };
        crate::cut_timing::measure("publisher authority", || {
            assert_publisher_session(&native_origin(&self.repo)?, lease, fence)
        })
    }
}

fn release_session(ctx: &CutCtx) -> Result<ReleaseSession> {
    if ctx.kind == CutKind::Draft {
        return Ok(ReleaseSession {
            repo: ctx.repo.clone(),
            guards: None,
            reservation: Some((ctx.commit.clone(), ctx.build)),
        });
    }
    if ctx.kind != CutKind::Real {
        return Ok(ReleaseSession {
            repo: ctx.repo.clone(),
            guards: None,
            reservation: None,
        });
    }
    let lease = ctx
        .lease
        .clone()
        .ok_or_else(|| Error::new("real release step has no acquired persistent claim lease"))?;
    let fence = ctx
        .fence
        .clone()
        .ok_or_else(|| Error::new("real release step has no unique publisher fence"))?;
    Ok(ReleaseSession {
        repo: ctx.repo.clone(),
        guards: Some((lease, fence)),
        reservation: None,
    })
}

fn ensure_upload_release_lease(ctx: &std::sync::Mutex<&mut CutCtx>) -> Result<()> {
    let session = upload_release_session(ctx)?;
    session.check()
}

fn upload_release_session(ctx: &std::sync::Mutex<&mut CutCtx>) -> Result<ReleaseSession> {
    release_session(&ctx.lock().unwrap())
}

fn revalidate_ctx_signature_policy(ctx: &CutCtx) -> Result<()> {
    if ctx.kind != CutKind::Real {
        return Ok(());
    }
    ensure_ctx_release_lease(ctx)?;
    let duty = roster_duty(ctx.is_done("sign"));
    let observed = preflight_signature_policy(ctx.credentials.as_ref(), duty)?;
    revalidate_observed_signature_policy(ctx, &observed, duty)?;
    ensure_ctx_release_lease(ctx)
}

/// Compare the fresh signing observation with this cut's captured inputs.
fn revalidate_observed_signature_policy(
    ctx: &CutCtx,
    observed: &SigningVerdict,
    duty: RosterDuty,
) -> Result<()> {
    if observed.policy.required != ctx.signature_required
        || observed.policy.pubkey.as_deref() != ctx.signature_pubkey.as_deref()
    {
        return Err(Error::new(
            "local signing configuration or the committed channel pin changed after this \
             cut's pre-claim scan; refusing to build/upload/flip under stale signing state",
        ));
    }
    if duty == RosterDuty::Sign {
        resume_attribution_agrees(
            ctx.signature_machine_id.as_deref(),
            observed.machine_id().as_deref(),
        )?;
        if observed.attribution != ctx.attribution
            || observed.roster.as_ref().map(|document| &document.bytes)
                != ctx.roster.as_ref().map(|document| &document.bytes)
        {
            return Err(Error::new(
                "current signing roster or attribution changed after this cut captured its \
                 signing inputs; refusing to stage or sign stale authority",
            ));
        }
    }
    Ok(())
}

/// Stage this cut's actual manifest and roster after the caller's fresh admission.
fn stage_ctx_signing_inputs(
    ctx: &CutCtx,
    inputs: &manifest_out::ManifestInputs<'_>,
    validate: impl FnOnce(&CutCtx) -> Result<()>,
) -> Result<PathBuf> {
    validate(ctx)?;
    let manifest = stage_manifest(
        &ctx.dist,
        inputs,
        ctx.attribution.as_ref(),
        ctx.linux.as_ref(),
        ctx.windows.as_ref(),
    )?;
    stage_roster_assets(&ctx.dist, ctx.roster.as_ref())?;
    Ok(manifest)
}

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

fn step_unlock(ctx: &mut CutCtx) -> Result<()> {
    let origin = native_origin(&ctx.repo)?;
    let outcome = if let Some(fence) = ctx.fence.as_ref() {
        release_completed_publisher_session(&origin, &ctx.commit, fence)?
    } else {
        release_completed_session_without_guard(&origin, &ctx.commit).map_err(|error| {
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

/// The two container builders and the two digest reads, behind a trait.
pub trait Packager {
    fn dmg(&self, app: &Path, dist: &Path, version: &str) -> Result<dmg::Packaged>;
    fn zip(&self, app: &Path, dist: &Path, version: &str) -> Result<dmg::Packaged>;
    fn sha256(&self, path: &Path) -> Result<String>;
    fn size(&self, path: &Path) -> Result<u64>;
    fn sha256_and_size(&self, path: &Path) -> Result<(String, u64)> {
        Ok((self.sha256(path)?, self.size(path)?))
    }
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
        crate::durable::open_regular_file_with_meta(path)
            .map(|(_, meta)| meta.len())
            .map_err(|e| Error::new(format!("stat {}: {e}", path.display())))
    }
    fn sha256_and_size(&self, path: &Path) -> Result<(String, u64)> {
        dmg::sha256_and_size(path).map_err(Error::new)
    }
}

/// Every container, and the digests that go on record for them.
pub struct PackagedCut {
    pub dmg: dmg::Packaged,
    pub dmg_sha256: String,
    pub dmg_size: u64,
    pub zip: dmg::Packaged,
}

/// The line said before the two notarization waits.
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

/// Notarize the bundle, build both containers around it, notarize the DMG, and return the digests that go on record.
pub fn notarize_and_package(
    app: &Path,
    dist: &Path,
    version: &str,
    tier: &sign::AppleTier,
    tools: &dyn sign::AppleTools,
    pack: &dyn Packager,
) -> Result<PackagedCut> {
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

    let dmg_out = pack.dmg(app, dist, version)?;
    let dmg_notarized =
        sign::sign_and_notarize_dmg(&dmg_out.path, tier, tools).map_err(Error::new)?;
    let (dmg_sha256, dmg_size) = if dmg_notarized {
        pack.sha256_and_size(&dmg_out.path)?
    } else {
        (dmg_out.sha256.clone(), dmg_out.size_bytes)
    };
    let zip = pack.zip(app, dist, version)?;
    Ok(PackagedCut {
        dmg: dmg_out,
        dmg_sha256,
        dmg_size,
        zip,
    })
}

fn spent_remedy(version: &str, build: u64) -> String {
    format!(
        "A claimed release is never rebuilt: build {build}'s number is spent, and nothing \
         rebuilt could be the bytes `verify release` judged. `{CUT_COMMAND} --abandon v{version}` \
         withdraws it; the next release reserves, builds and measures anew"
    )
}

/// Write `notes-<version>.md` and return the body it carries: the release commit's rolled
/// section read from its objects ([`release_body_of`]), the same bytes in every cut mode and
/// whatever the working tree's `CHANGELOG.md` holds.
fn write_release_notes(ctx: &CutCtx) -> Result<String> {
    let body = release_body_of(&ledger::GitCli::new(&ctx.tree), &ctx.commit, &ctx.version)?;
    crate::durable::write_fresh(
        &ctx.notes_path(),
        changelog::release_notes_document(&ctx.version, &body),
    )
    .map_err(|e| Error::new(format!("write {}: {e}", ctx.notes_path().display())))?;
    Ok(body)
}

fn step_sign(ctx: &mut CutCtx) -> Result<()> {
    if ctx.kind == CutKind::Real && draft::staged(ctx)? {
        return Ok(());
    }
    if let Some(handoff) = &mut ctx.linux {
        handoff.run_workers(&ctx.repo, &ctx.version, ctx.build, &ctx.commit)?;
        handoff.import(&ctx.dist, &ctx.version, ctx.build, &ctx.commit)?;
        if let Some(journal) = &mut ctx.journal {
            journal.linux = Some(handoff.clone());
            journal.save(&ctx.journal_path)?;
        }
        match handoff.record(&ctx.slug, &ctx.version, ctx.build, &ctx.commit) {
            None => println!(
                "==> no static bootstrap verifier in this cut: install.sh keeps its embedded \
                 verifier"
            ),
            Some(record) => {
                let at = ctx.dist.join(crate::buildplan::linux::PIN_RECORD);
                crate::durable::write_fresh(&at, record.to_toml())
                    .map_err(|e| Error::new(format!("write {}: {e}", at.display())))?;
                println!(
                    "==> install.sh's bootstrap verifier pin for v{} is in {}, and the signed \
                     appcast carries its digests. Once the release is public, `targo \
                     --unverified ship pin-verifier --from-release v{}` in any checkout of main \
                     (or `ship pin-verifier {}` here) writes these lines into tools/install.sh, \
                     with {}; commit both:\n{}",
                    ctx.version,
                    at.display(),
                    ctx.version,
                    at.display(),
                    aterm_bootstrap_verify::pin::RECORD,
                    record.pin().lines()
                );
            }
        }
    }
    if let Some(handoff) = &mut ctx.windows {
        // The commit's tree and pins, read from this repository: what the receipt must name.
        let sources = buildplan::windows::Sources::at(&GitCli::new(&ctx.repo), &ctx.commit)?;
        handoff.import(&ctx.dist, &ctx.version, ctx.build, &ctx.commit, &sources)?;
        if let Some(journal) = &mut ctx.journal {
            journal.windows = Some(handoff.clone());
            journal.save(&ctx.journal_path)?;
        }
        if let Some(artifact) = &handoff.artifact {
            println!(
                "==> Windows zip {} admitted: {} bytes, sha256 {}, built in the {} lane",
                artifact.asset, artifact.size, artifact.sha256, artifact.lane
            );
        }
    }
    let source = signing_source(ctx)?;
    let copy = match adopted_copy(ctx, &source)? {
        Some(copy) => {
            ctx.adopted = true;
            copy
        }
        None => sign_copy(ctx, &source)?,
    };
    let SigningSource {
        published,
        macho_sha256,
        ..
    } = source;
    let body = write_release_notes(ctx)?;

    let plist_text = gates::read_bounded_string(&copy.app.join("Contents/Info.plist"), 16 << 20)
        .map_err(|e| Error::new(format!("read stamped Info.plist: {e}")))?;
    let min_os = manifest_out::plist_string(&plist_text, "LSMinimumSystemVersion")
        .unwrap_or_else(|| "11.0".to_string());
    let inputs = manifest_out::ManifestInputs {
        version: &ctx.version,
        build_number: ctx.build,
        commit: &ctx.commit,
        dmg_name: &channel::dmg_asset_name(&ctx.version),
        dmg_sha256: &copy.dmg_sha256,
        zip_name: &channel::zip_asset_name(&ctx.version),
        zip_sha256: &copy.zip_sha256,
        repo_slug: &ctx.slug,
        min_os: &min_os,
        team_id: aterm_update_core::pins::APPLE_TEAM_ID,
        pub_date: &bundle::epoch_to_rfc3339(unix_now()),
        min_build: ctx.min_build,
        changelog: &body,
    };
    let mpath = stage_ctx_signing_inputs(ctx, &inputs, revalidate_ctx_signature_policy)?;
    crate::service_qualification::produce(
        &ctx.repo,
        &published,
        &macho_sha256,
        &copy.app,
        &copy.dmg,
        ctx.build,
        &ctx.commit,
        &mpath,
    )?;
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
        let produced = sign_ctx_manifest(ctx, &mpath, revalidate_ctx_signature_policy)?;
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
        journal.manifest_signed = ctx.manifest_signed;
        journal.signature_required = ctx.signature_required;
        journal.signature_pubkey.clone_from(&ctx.signature_pubkey);
        journal.verify_pubkey.clone_from(&ctx.verify_pubkey);
        journal
            .signature_machine_id
            .clone_from(&ctx.signature_machine_id);
    }
    if ctx.kind == CutKind::Real {
        let slug = ctx.slug.clone();
        channel::stage_on_channel(&mut LiveChannelRelease {
            ctx,
            slug: &slug,
            bound: None,
        })?;
    }
    Ok(())
}

/// The built bundle a cut signs a copy of: its COMPLETE record in the release cache, that
/// cache, its executable's digest, and R, the commit its `verify release` records are of.
struct SigningSource {
    complete: artifact::Complete,
    store: PathBuf,
    macho_sha256: String,
    published: String,
}

/// The cut's verified bundle, re-proved: the cache still holds the COMPLETE bundle the cut names,
/// built as this cut's version, number and commit, and every check's record of R stands for its
/// executable (NOTHING SHIPS UNVERIFIED).
fn signing_source(ctx: &CutCtx) -> Result<SigningSource> {
    signing_source_with_verification(ctx, true)
}

fn signing_source_with_verification(ctx: &CutCtx, require_verified: bool) -> Result<SigningSource> {
    let _timing = crate::cut_timing::Timer::new("signing source reproof");
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
    let published = rev_parse(&GitCli::new(&ctx.tree), &format!("{}^1", ctx.commit))?;
    if require_verified {
        let (yes, line) = gates::verified(&GitCli::new(&ctx.repo), &published, &macho_sha256)?;
        if !yes {
            return Err(Error::new(format!(
                "NOTHING SHIPS UNVERIFIED: {line} — refusing to sign build {}",
                ctx.build
            )));
        }
        step("gate", &line);
    }
    Ok(SigningSource {
        complete,
        store,
        macho_sha256,
        published,
    })
}

/// A signed, notarized and packaged copy of the verified bundle in `dist/`: the staged bundle,
/// both containers and their digests.
struct SignedCopy {
    app: PathBuf,
    dmg: PathBuf,
    dmg_sha256: String,
    zip_sha256: String,
}

/// Copy the verified bundle byte for byte into this cut's staging, sign it, notarize and staple
/// it, package and notarize its DMG, zip it, and stage the stable twins, the provenance and the
/// `.sha256` sidecars: everything of the cut that is private and keyed by the bundle's bytes. A
/// record of an earlier copy of this build goes first, so none outlives the bytes it names.
fn sign_copy(ctx: &CutCtx, source: &SigningSource) -> Result<SignedCopy> {
    forget_signed_record(&ctx.dist, ctx.build)?;
    let SigningSource {
        complete,
        store,
        macho_sha256,
        ..
    } = source;
    let app = bundle::prepare_staging(&ctx.dist, ctx.build).map_err(Error::new)?;
    artifact::copy_bundle(complete, &complete.app(store), &app).map_err(Error::new)?;
    artifact::place_records(&complete.dir(store), &ctx.dist, &ctx.version).map_err(Error::new)?;
    step(
        "bundle",
        &format!(
            "the built bundle, byte for byte: {} → {} — executable sha256 {}… (archs [{}] · \
             {}) · Short={} CFBundleVersion={}",
            complete.app(store).display(),
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
    validate_release_asset_download_size(dmg_size)?;
    validate_lean_dmg_size(&ctx.version, dmg_size)?;
    let provenance_path = bundle::write_provenance(&spec, &app, &signed_by, macho_sha256)?;
    if ctx.signature_required {
        let fingerprint = ctx
            .expected_embedded_pin()?
            .ok_or_else(|| Error::new("signed build has no pinned paper master"))?;
        let mut provenance =
            gates::read_bounded_string(&provenance_path, 16 << 20).map_err(|error| {
                Error::new(format!(
                    "read {} for update-pin provenance: {error}",
                    provenance_path.display()
                ))
            })?;
        use std::fmt::Write as _;
        let _ = writeln!(provenance, "update_pubkey_fingerprint_sha256={fingerprint}");
        crate::durable::write_fresh(&provenance_path, provenance).map_err(|error| {
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
    stage_twins_and_sidecars(ctx, &dout.path, &dmg_sha, &zout.path, &zout.sha256)?;
    Ok(SignedCopy {
        app,
        dmg: dout.path,
        dmg_sha256: dmg_sha,
        zip_sha256: zout.sha256,
    })
}

/// The stable twins of both containers (the evergreen `releases/latest/download` names) and the
/// `.sha256` sidecar of every container and twin: copies and digests of the containers alone.
fn stage_twins_and_sidecars(
    ctx: &CutCtx,
    dmg: &Path,
    dmg_sha: &str,
    zip: &Path,
    zip_sha: &str,
) -> Result<()> {
    let _timing = crate::cut_timing::Timer::new("container twins and sidecars");
    for (source, twin) in [(dmg, ctx.stable_dmg_path()), (zip, ctx.stable_zip_path())] {
        crate::durable::copy_regular_fresh(source, &twin).map_err(|e| {
            Error::new(format!(
                "copy {} -> {}: {e}",
                source.display(),
                twin.display()
            ))
        })?;
    }
    let sidecars = [
        (
            ctx.dmg_sha256_path(),
            dmg_sha,
            channel::dmg_asset_name(&ctx.version),
        ),
        (
            ctx.zip_sha256_path(),
            zip_sha,
            channel::zip_asset_name(&ctx.version),
        ),
        (
            ctx.stable_dmg_sha256_path(),
            dmg_sha,
            channel::stable_dmg_asset_name(),
        ),
        (
            ctx.stable_zip_sha256_path(),
            zip_sha,
            channel::stable_zip_asset_name(),
        ),
    ];
    for (path, sha, name) in sidecars {
        crate::durable::write_fresh(&path, channel::sha256_sidecar_contents(sha, &name))
            .map_err(|e| Error::new(format!("write {}: {e}", path.display())))?;
    }
    step(
        "",
        "`.sha256` sidecars staged for every container and every stable twin \
         (shasum -a 256 -c)",
    );
    Ok(())
}

/// The copy `cut --sign` signed of `source` for this cut, when its record is this cut's
/// ([`signed_key`]) and `dist/` still holds every output it names byte for byte
/// ([`signed_copy_stands`]): the build's records are placed from the cache again and the twins and
/// sidecars staged from the containers, and nothing is signed or notarized again. A record that
/// does not stand is removed, and `None` has the copy signed again.
fn adopted_copy(ctx: &CutCtx, source: &SigningSource) -> Result<Option<SignedCopy>> {
    let _timing = crate::cut_timing::Timer::new("adopt signed bytes");
    let Some(record) = read_signed_record(&ctx.dist, ctx.build).transpose() else {
        return Ok(None);
    };
    let key = signed_key(ctx, &source.complete)?;
    let Some(record) = standing_record(record, &key, &ctx.dist)? else {
        return Ok(None);
    };
    place_signed_copy(ctx, source, &record).map(Some)
}

/// A prepared copy has passed no runtime checks. Only `cut --sign`, after verification,
/// may promote it to the signed record the claim reads. Mismatched bytes refuse here.
fn prepared_copy(ctx: &CutCtx, source: &SigningSource) -> Result<Option<SignedCopy>> {
    let Some(record) = prepared_record(&ctx.dist, &signed_key(ctx, &source.complete)?)? else {
        return Ok(None);
    };
    place_signed_copy(ctx, source, &record).map(Some)
}

fn prepared_record(dist: &Path, key: &SignedKey) -> Result<Option<SignedRecord>> {
    let path = prepared_record_path(dist, key.build);
    let Some(record) = read_copy_record(&path).map_err(Error::new)? else {
        return Ok(None);
    };
    signed_copy_stands(&record, key, dist)
        .map_err(|why| Error::new(format!("early signature refused: {why}")))?;
    Ok(Some(record))
}

fn place_signed_copy(
    ctx: &CutCtx,
    source: &SigningSource,
    record: &SignedRecord,
) -> Result<SignedCopy> {
    artifact::place_records(&source.complete.dir(&source.store), &ctx.dist, &ctx.version)
        .map_err(Error::new)?;
    let app = ctx.app_path();
    let (dmg, zip) = (ctx.dmg_path(), ctx.zip_path());
    let outputs = &record.outputs;
    step(
        "adopt",
        &format!(
            "the copy `cut --sign` signed{} of these verified bytes, every digest re-proved: \
             {} (bundle {}…) · DMG sha256 {}… · zip sha256 {}… — nothing is signed or notarized \
             again",
            if record.key.identity.is_some() {
                ", notarized and stapled"
            } else {
                ""
            },
            app.display(),
            &outputs.app_digest[..12.min(outputs.app_digest.len())],
            &outputs.dmg_sha256[..12.min(outputs.dmg_sha256.len())],
            &outputs.zip_sha256[..12.min(outputs.zip_sha256.len())]
        ),
    );
    stage_twins_and_sidecars(ctx, &dmg, &outputs.dmg_sha256, &zip, &outputs.zip_sha256)?;
    Ok(SignedCopy {
        app,
        dmg,
        dmg_sha256: outputs.dmg_sha256.clone(),
        zip_sha256: outputs.zip_sha256.clone(),
    })
}

/// The record `cut --sign` keeps of the copy it signed, in that copy's staging directory
/// (`dist/cut-<n>.noindex/signed.toml`): it goes wherever the staged bundle goes, and the claim
/// adopts it ([`adopted_copy`]).
pub const SIGNED_RECORD: &str = "signed.toml";

/// [`SignedRecord::format`] this cutter writes and adopts.
pub const SIGNED_FORMAT: u32 = 1;

/// A signed copy of a verified bundle: what it is a copy of and under what ([`SignedKey`]), and
/// the digest of everything it left in `dist/` ([`SignedOutputs`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRecord {
    pub format: u32,
    pub key: SignedKey,
    pub outputs: SignedOutputs,
}

/// What a signed copy is a copy of, and under what identity: a claim adopts the copy only when
/// every field is its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedKey {
    /// The verified bundle's [`artifact::Complete::key`].
    pub artifact: String,
    /// Its [`artifact::Complete::bundle_digest`]: the bytes copied, before they were signed.
    pub bundle_digest: String,
    /// SHA-256 of its executable, the record `verify release` passed.
    pub macho_sha256: String,
    pub version: String,
    /// The reserved number n the bytes carry.
    pub build: u64,
    /// The reserved release commit C.
    pub commit: String,
    /// The team the copy was signed for (`pins::APPLE_TEAM_ID`; empty ad hoc).
    pub team: String,
    /// The Developer ID's SHA-1; `None` signed ad hoc.
    pub identity: Option<String>,
    /// The build requires a signed manifest, and its provenance carries the update pin then.
    pub signature_required: bool,
    /// That pin's fingerprint.
    pub update_pin: Option<String>,
}

/// The digests of what a signed copy left in `dist/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedOutputs {
    /// [`artifact::bundle_digest`] of the signed, stapled bundle in its staging directory.
    pub app_digest: String,
    pub dmg_sha256: String,
    pub dmg_size: u64,
    pub zip_sha256: String,
    pub zip_size: u64,
    /// SHA-256 of the provenance record.
    pub provenance_sha256: String,
}

fn signed_record_path(dist: &Path, build: u64) -> PathBuf {
    dist.join(bundle::staging_dir_name(build))
        .join(SIGNED_RECORD)
}

fn prepared_record_path(dist: &Path, build: u64) -> PathBuf {
    signed_record_path(dist, build).with_file_name("prepared.toml")
}

/// Build `build`'s signed record in `dist/`: `Ok(None)` without one, `Err` naming why one is
/// there and unreadable.
fn read_signed_record(
    dist: &Path,
    build: u64,
) -> std::result::Result<Option<SignedRecord>, String> {
    read_copy_record(&signed_record_path(dist, build))
}

fn read_copy_record(path: &Path) -> std::result::Result<Option<SignedRecord>, String> {
    match crate::durable::read_regular_text(path, 1 << 20) {
        Ok(text) => aterm_toml::from_str(&text)
            .map(Some)
            .map_err(|e| format!("{} does not parse: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

fn write_signed_record(dist: &Path, record: &SignedRecord) -> Result<()> {
    let path = signed_record_path(dist, record.key.build);
    write_copy_record(&path, record)
}

fn write_copy_record(path: &Path, record: &SignedRecord) -> Result<()> {
    let text = aterm_toml::to_string(record)
        .map_err(|e| Error::new(format!("serialize the signed record: {e}")))?;
    crate::durable::write_atomic_durable(path, text.as_bytes(), 0o644)
        .map_err(|e| Error::new(format!("write {}: {e}", path.display())))
}

/// `record`, as [`read_signed_record`] read build `key.build`'s, when it stands for `key` over
/// `dist`; else its record is removed and the copy is signed again. A record that cannot be read
/// is no record of these bytes.
fn standing_record(
    record: std::result::Result<SignedRecord, String>,
    key: &SignedKey,
    dist: &Path,
) -> Result<Option<SignedRecord>> {
    let why = match record.and_then(|r| signed_copy_stands(&r, key, dist).map(|()| r)) {
        Ok(record) => return Ok(Some(record)),
        Err(why) => why,
    };
    step(
        "adopt",
        &format!(
            "the signed copy of build {} is not adopted: {why} — its record is removed and the \
             copy signed again",
            key.build
        ),
    );
    forget_signed_record(dist, key.build)?;
    Ok(None)
}

/// The outputs `cut --sign` records are the ones it signed and packaged: `dist/` changed under
/// it otherwise, and nothing is recorded.
fn outputs_as_signed(outputs: &SignedOutputs, signed: &SignedCopy) -> Result<()> {
    for (what, recorded, made) in [
        ("DMG", &outputs.dmg_sha256, &signed.dmg_sha256),
        ("zip", &outputs.zip_sha256, &signed.zip_sha256),
    ] {
        if recorded != made {
            return Err(Error::new(format!(
                "the {what} in dist/ is {recorded}, and `cut --sign` made {made}: dist/ changed \
                 while it signed, so nothing is recorded and the claim signs again"
            )));
        }
    }
    Ok(())
}

fn forget_signed_record(dist: &Path, build: u64) -> Result<()> {
    forget_copy_record(&signed_record_path(dist, build))
}

fn forget_copy_record(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::new(format!("remove {}: {e}", path.display()))),
    }
}

/// The key of the copy this cut signs of `complete`.
fn signed_key(ctx: &CutCtx, complete: &artifact::Complete) -> Result<SignedKey> {
    Ok(SignedKey {
        artifact: complete.key.clone(),
        bundle_digest: complete.bundle_digest.clone(),
        macho_sha256: complete.macho_sha256.clone(),
        version: ctx.version.clone(),
        build: ctx.build,
        commit: ctx.commit.clone(),
        team: aterm_update_core::pins::APPLE_TEAM_ID.to_string(),
        identity: ctx.apple.identity().map(|id| id.sha1.clone()),
        signature_required: ctx.signature_required,
        update_pin: if ctx.signature_required {
            ctx.expected_embedded_pin()?
        } else {
            None
        },
    })
}

/// What `dist/` holds now as the signed copy of build `build` of `version`, by digest.
fn signed_outputs(
    dist: &Path,
    version: &str,
    build: u64,
) -> std::result::Result<SignedOutputs, String> {
    let _timing = crate::cut_timing::Timer::new("signed output digest reproofs");
    let sized = |path: PathBuf| -> std::result::Result<(String, u64), String> {
        let size = fs::symlink_metadata(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .len();
        Ok((artifact::file_sha256(&path)?, size))
    };
    let (dmg_sha256, dmg_size) = crate::cut_timing::measure("signed DMG digest", || {
        sized(dist.join(channel::dmg_asset_name(version)))
    })?;
    let (zip_sha256, zip_size) = crate::cut_timing::measure("signed zip digest", || {
        sized(dist.join(channel::zip_asset_name(version)))
    })?;
    Ok(SignedOutputs {
        app_digest: crate::cut_timing::measure("signed bundle digest", || {
            artifact::bundle_digest(&bundle::staged_app_path(dist, build))
        })?,
        dmg_sha256,
        dmg_size,
        zip_sha256,
        zip_size,
        provenance_sha256: artifact::file_sha256(
            &dist.join(channel::provenance_asset_name(version)),
        )?,
    })
}

/// Whether `record` is the record of the copy `key` names and `dist/` still holds that copy
/// byte for byte; `Err` names the first field that differs.
pub(crate) fn signed_copy_stands(
    record: &SignedRecord,
    key: &SignedKey,
    dist: &Path,
) -> std::result::Result<(), String> {
    if record.format != SIGNED_FORMAT {
        return Err(format!(
            "its record is format {}, and this cutter adopts format {SIGNED_FORMAT}",
            record.format
        ));
    }
    let (was, is) = (&record.key, key);
    let keys = [
        ("bundle", was.artifact.clone(), is.artifact.clone()),
        (
            "bundle digest",
            was.bundle_digest.clone(),
            is.bundle_digest.clone(),
        ),
        (
            "executable",
            was.macho_sha256.clone(),
            is.macho_sha256.clone(),
        ),
        ("version", was.version.clone(), is.version.clone()),
        ("build", was.build.to_string(), is.build.to_string()),
        ("release commit", was.commit.clone(), is.commit.clone()),
        ("team", was.team.clone(), is.team.clone()),
        (
            "Developer ID",
            format!("{:?}", was.identity),
            format!("{:?}", is.identity),
        ),
        (
            "manifest signature requirement",
            was.signature_required.to_string(),
            is.signature_required.to_string(),
        ),
        (
            "update pin",
            format!("{:?}", was.update_pin),
            format!("{:?}", is.update_pin),
        ),
    ];
    if let Some((field, was, is)) = keys.iter().find(|(_, was, is)| was != is) {
        return Err(format!(
            "it was signed as {field} {was}, and this cut's is {is}"
        ));
    }
    let now = signed_outputs(dist, &key.version, key.build)
        .map_err(|why| format!("its outputs cannot be read again ({why})"))?;
    let (was, is) = (&record.outputs, &now);
    let outputs = [
        (
            "the signed bundle",
            was.app_digest.clone(),
            is.app_digest.clone(),
        ),
        ("the DMG", was.dmg_sha256.clone(), is.dmg_sha256.clone()),
        (
            "the DMG's size",
            was.dmg_size.to_string(),
            is.dmg_size.to_string(),
        ),
        ("the zip", was.zip_sha256.clone(), is.zip_sha256.clone()),
        (
            "the zip's size",
            was.zip_size.to_string(),
            is.zip_size.to_string(),
        ),
        (
            "the provenance",
            was.provenance_sha256.clone(),
            is.provenance_sha256.clone(),
        ),
    ];
    if let Some((what, was, is)) = outputs.iter().find(|(_, was, is)| was != is) {
        return Err(format!(
            "{what} is {is} on disk, and the record names {was}"
        ));
    }
    Ok(())
}

/// Discard build `build`'s privately signed copy of `version` from `dist/`: its record, its
/// staging directory, both containers, their stable twins and sidecars, and its provenance.
/// Cancellation retains a draft journaled for this version and build, and its bytes; a journal of
/// another build owns none of this build's preparation. Only a lost reservation invalidates a
/// journaled copy: its draft is withdrawn while release.toml still reserves that build
/// ([`draft::discard`]), so a release dropping a reservation discards before it journals another
/// or forgets the journal. A draft that discard holds keeps everything (`Held`, with what a
/// person does); else what could not be removed (the next cut's staging prunes a staging
/// directory a live process held).
pub(crate) fn discard_signed(
    dist: &Path,
    version: &str,
    build: u64,
    reason: crate::release::SignedDiscard,
) -> crate::release::Discarded {
    use crate::release::Discarded;
    let retain = match reason {
        crate::release::SignedDiscard::CancelPreparation => {
            draft::has_journal_for(dist, version, build).map_err(draft::Kept::Failed)
        }
        crate::release::SignedDiscard::LostReservation => {
            draft::discard(dist, version, build).map(|()| false)
        }
    };
    let mut left = Vec::new();
    match retain {
        Ok(true) => return Discarded::Except(left),
        Ok(false) => {}
        Err(draft::Kept::Failed(error)) => return Discarded::Except(vec![error.to_string()]),
        Err(draft::Kept::Held { why, remedy }) => return Discarded::Held { why, remedy },
    }
    if let Err(error) = forget_signed_record(dist, build) {
        left.push(error.to_string());
    }
    if let Err(error) = forget_copy_record(&prepared_record_path(dist, build)) {
        left.push(error.to_string());
    }
    if let Err(why) = bundle::discard_staged_app(dist, build) {
        left.push(why);
    }
    let dmg = channel::dmg_asset_name(version);
    let zip = channel::zip_asset_name(version);
    let stable_dmg = channel::stable_dmg_asset_name();
    let stable_zip = channel::stable_zip_asset_name();
    let files = [
        channel::sha256_sidecar_name(&dmg),
        channel::sha256_sidecar_name(&zip),
        channel::sha256_sidecar_name(&stable_dmg),
        channel::sha256_sidecar_name(&stable_zip),
        dmg,
        zip,
        stable_dmg,
        stable_zip,
        channel::provenance_asset_name(version),
    ];
    for name in files {
        let path = dist.join(name);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => left.push(format!("remove {}: {e}", path.display())),
        }
    }
    Discarded::Except(left)
}

/// `repo`'s private-draft journal, `dist/draft-state.toml`: what `cut --sign` writes and the
/// engine's `--draft-state` reads.
pub(crate) fn draft_journal_path(repo: &Path) -> PathBuf {
    repo.join("dist").join(draft::JOURNAL)
}

/// The engine's `sign-release` arguments that attach `version`'s source attestation pair to the
/// release it belongs on. Only `--keep-draft --draft-state <journal>` reaches a draft: the engine
/// reads the journal's release id (waiting while it names none), version, create intent and
/// commit (whose parent must be the published source; it reads no build), attaches and verifies
/// the pair on that draft and never publishes it. `draft_state` is a journal that binds the draft:
/// the private draft's `cut --sign` journaled ([`draft_journal_path`]), or a claimed cut's own.
/// Without it the engine takes the tag's release, creating one or publishing a draft it finds,
/// so the plain form is for a published release alone.
pub(crate) fn sign_release_args(
    repo: &Path,
    version: &str,
    draft_state: Option<&Path>,
) -> Vec<String> {
    let mut args = vec![
        "sign-release".to_string(),
        "aterm".to_string(),
        "--tag".to_string(),
        format!("v{version}"),
    ];
    if let Some(journal) = draft_state {
        args.extend([
            "--keep-draft".to_string(),
            "--draft-state".to_string(),
            journal.display().to_string(),
            "--repo-path".to_string(),
            repo.display().to_string(),
        ]);
    }
    args
}

/// [`sign_release_args`] as the command a person runs.
pub(crate) fn sign_release_command(
    repo: &Path,
    version: &str,
    draft_state: Option<&Path>,
) -> String {
    format!(
        "pub {}",
        sign_release_args(repo, version, draft_state).join(" ")
    )
}

/// The private draft `cut --sign` journaled in `dist` for build `build` of `version`: its
/// release id, or why the journal cannot be read.
pub(crate) fn journaled_draft(
    dist: &Path,
    version: &str,
    build: u64,
) -> std::result::Result<Option<u64>, String> {
    draft::journaled_release(dist, version, build).map_err(|e| e.to_string())
}

/// The self-check of a copy `cut --sign` signed and the claim adopted: the manifest's `team` is
/// the one the copy was signed for (`pinned`), and its signing half is asked again, now, since a
/// ticket or a certificate can be revoked between the sign and the claim.
/// # Errors
/// Another team, and what [`selfcheck_signing`] refuses.
pub fn adopted_selfcheck(
    team: &str,
    pinned: &str,
    app: &Path,
    dmg: &Path,
    tools: &dyn sign::AppleTools,
) -> Result<&'static str> {
    if team != pinned {
        return Err(Error::new(format!(
            "self-check failed: the manifest names team {team:?}, and the adopted copy was \
             signed for {pinned:?}"
        )));
    }
    selfcheck_signing(team, app, dmg, tools)
}

/// The signing half of the self-check.
pub fn selfcheck_signing(
    team: &str,
    app: &Path,
    dmg: &Path,
    tools: &dyn sign::AppleTools,
) -> Result<&'static str> {
    crate::cut_timing::measure("codesign verify", || tools.codesign_verify_strict(app)).map_err(
        |e| {
            Error::new(format!(
                "self-check failed: codesign --verify --deep --strict: {e}"
            ))
        },
    )?;
    if team.is_empty() {
        return Ok("");
    }
    sign::apple_selfcheck_verdict(&sign::AppleSelfcheck {
        team_id: team,
        app_codesign_dv: &crate::cut_timing::measure("codesign identity", || {
            tools.codesign_dv(app)
        })
        .map_err(Error::new)?,
        app_stapled: crate::cut_timing::measure("app stapled ticket", || {
            tools.stapler_validate(app)
        })
        .map_err(Error::new)?,
        app_gatekeeper_ok: crate::cut_timing::measure("app Gatekeeper", || {
            tools.gatekeeper_ok(app, sign::GatekeeperKind::App)
        })
        .map_err(Error::new)?,
        dmg_stapled: crate::cut_timing::measure("dmg stapled ticket", || {
            tools.stapler_validate(dmg)
        })
        .map_err(Error::new)?,
        dmg_gatekeeper_ok: crate::cut_timing::measure("dmg Gatekeeper", || {
            tools.gatekeeper_ok(dmg, sign::GatekeeperKind::Dmg)
        })
        .map_err(Error::new)?,
    })
    .map_err(Error::new)?;
    Ok(" · Tier APPLE: TeamIdentifier + stapled ticket + Gatekeeper on .app and .dmg")
}

fn prove_artifacts_on_disk(ctx: &CutCtx) -> Result<Manifest> {
    prove_bundle_on_disk(ctx)?;
    prove_publication_on_disk(ctx)
}

fn prove_bundle_on_disk(ctx: &CutCtx) -> Result<()> {
    let app = ctx.app_path();

    let plist_text = gates::read_bounded_string(&app.join("Contents/Info.plist"), 16 << 20)
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

fn prove_publication_on_disk(ctx: &CutCtx) -> Result<Manifest> {
    let _timing = crate::cut_timing::Timer::new("publication bytes and manifest");
    if ctx.artifact.is_some() {
        crate::service_qualification::verify(&ctx.app_path(), &ctx.manifest_path())?;
    } else {
        if ctx.kind != CutKind::Real || !ctx.is_done("selfcheck") {
            return Err(Error::new(
                "an unbuilt candidate is not a published recovery snapshot",
            ));
        }
        let release_id = ctx
            .release_id
            .ok_or_else(|| Error::new("recovery snapshot has no bound release ID"))?;
        let published = unique_release_object_by_tag(&ctx.slug, &ctx.tag)?;
        validate_channel_release_capability(published.as_ref(), release_id, &ctx.tag, false)?;
        crate::service_qualification::verify_published_report(&ctx.manifest_path())?;
    }
    if let Some(handoff) = &ctx.linux {
        handoff.verify_staged(&ctx.dist, &ctx.version, ctx.build, &ctx.commit)?;
    }
    if let Some(handoff) = &ctx.windows {
        handoff.verify_staged(&ctx.dist, &ctx.version, ctx.build, &ctx.commit)?;
    }
    validate_lean_dmg_on_disk(
        &ctx.dmg_path(),
        "self-check",
        Some(&format!(
            "`{CUT_COMMAND} --abandon v{}` and cut again",
            ctx.version
        )),
    )?;

    if ctx.artifact.is_some() {
        let provenance = gates::read_bounded_bytes(&ctx.provenance_path(), 16 << 20)
            .map_err(|error| Error::new(format!("read release provenance: {error}")))?;
        validate_claim_provenance(&provenance, &ctx.version, ctx.build, &ctx.commit)?;

        if ctx.signature_required {
            let fingerprint = ctx
                .expected_embedded_pin()?
                .ok_or_else(|| Error::new("signed channel has no pinned paper master"))?;
            let provenance_text = std::str::from_utf8(&provenance)
                .map_err(|_| Error::new("release provenance is not UTF-8"))?;
            let expected = format!("update_pubkey_fingerprint_sha256={fingerprint}");
            if !provenance_text.lines().any(|line| line == expected) {
                return Err(Error::new(format!(
                    "release provenance is missing exact update-pin field {expected:?}"
                )));
            }
        }
    }

    let mtext = gates::read_bounded_string(&ctx.manifest_path(), 16 << 20)
        .map_err(|e| Error::new(format!("read {}: {e}", ctx.manifest_path().display())))?;
    let manifest = Manifest::parse(&mtext)
        .map_err(|e| Error::new(format!("self-check: manifest re-parse failed: {e}")))?;
    buildplan::linux::verify_manifest_handoff(&manifest, ctx.linux.as_ref())?;
    buildplan::windows::verify_manifest_handoff(&manifest, ctx.windows.as_ref())?;
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
        let signature = gates::read_bounded_bytes(&sig_path, 16 << 20)
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

    let sha = dmg::sha256_file(&ctx.dmg_path())?;
    if !sha.eq_ignore_ascii_case(&manifest.sha256) {
        return Err(Error::new(format!(
            "self-check failed: DMG sha256 {sha} != manifest {}",
            manifest.sha256
        )));
    }

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

    refuse_retired_intel_dmg(&manifest)?;

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
            crate::durable::copy_regular_fresh(&source, &twin).map_err(|e| {
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
            crate::durable::write_fresh(&path, &expected)
                .map_err(|e| Error::new(format!("self-check: write {}: {e}", path.display())))?;
        }
        let observed = gates::read_bounded_string(&path, 16 << 20)
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
    prove_roster_on_disk(ctx, &manifest)?;
    Ok(manifest)
}

fn step_selfcheck(ctx: &mut CutCtx) -> Result<()> {
    // SIGN-BEFORE-SELFCHECK and SELFCHECK-BEFORE-TAG remain pipeline edges.
    // The signed local bytes and the public floor are independent read sets.
    let (bytes, best) = crate::cut_timing::independent(
        ("signed bytes and identity", || selfcheck_bytes(ctx)),
        ("published floor", || best_published(ctx)),
    );
    bytes?;
    let best = best?;
    step(
        "",
        &format!(
            "manifest bytes parse (shared type + vendored v0.25 fixture) · > published {}",
            best.map_or("none".to_string(), |b| b.to_string())
        ),
    );
    Ok(())
}

fn selfcheck_bytes(ctx: &CutCtx) -> Result<()> {
    let manifest = prove_artifacts_on_disk(ctx)?;
    let team = manifest.team_id.clone().unwrap_or_default();
    if ctx.adopted {
        let apple_note = crate::cut_timing::measure("adopted codesign and Gatekeeper", || {
            adopted_selfcheck(
                &team,
                aterm_update_core::pins::APPLE_TEAM_ID,
                &ctx.app_path(),
                &ctx.dmg_path(),
                &sign::RealAppleTools,
            )
        })?;
        step(
            "selfcheck",
            &format!(
                "binary == plist == manifest == {} · the copy's executions and its GPU session \
                 passed in `cut --sign` on these bytes, re-proved by digest at the sign step · \
                 codesign --verify --deep --strict ok{apple_note}",
                ctx.build
            ),
        );
    } else {
        let apple_note = selfcheck_local(ctx, &team)?;
        step(
            "selfcheck",
            &format!(
                "binary == plist == manifest == {} · codesign --verify --deep --strict \
                 ok{apple_note}",
                ctx.build
            ),
        );
    }

    Ok(())
}

/// The self-check's half that runs the staged copy and asks only this Mac: its first exec's
/// build stamp, each tool's `--version`, the update pin it reports, a session on the GPU, and
/// its signature, ticket and Gatekeeper verdicts for `team`. Returns the signing note.
fn selfcheck_local(ctx: &CutCtx, team: &str) -> Result<&'static str> {
    let app = ctx.app_path();

    // The staged copy's first exec, stamped in its staging directory before it: only an exit
    // of its own shows it ran, and the staging stays until then (`bundle::unproven`).
    let staging = ctx.dist.join(bundle::staging_dir_name(ctx.build));
    let _ = fs::remove_file(staging.join(aterm_uds::deadline::RAN));
    aterm_uds::deadline::stamp_exec(&staging);
    let diag = command(app.join("Contents/MacOS/aterm"))
        .arg("--diagnose")
        .current_dir(&ctx.repo)
        .output()
        .map_err(|e| Error::new(format!("spawn aterm --diagnose: {e}")))?;
    if diag.status.code().is_some() {
        let _ = aterm_uds::deadline::try_mark_ran(&staging);
    }
    if !diag.status.success() {
        return Err(Error::new(format!(
            "self-check failed: the shipped binary's --diagnose probe exited {}",
            diag.status
        )));
    }
    let diag_text = String::from_utf8_lossy(&diag.stdout).into_owned();
    buildplan::validate_app_version_reports(&ctx.version, &[("shipped universal", &diag_text)])?;
    let expect_built = bundle::epoch_to_rfc3339(ctx.build);
    let built = diag_text
        .lines()
        .find_map(|l| l.split_once("built ").map(|(_, t)| t.trim_end_matches(')')));
    if built != Some(expect_built.as_str()) {
        return Err(Error::new(format!(
            "self-check failed: binary build stamp {built:?} != expected {expect_built:?} \
             (from claimed n {}) — the binary was not compiled with this claim",
            ctx.build
        )));
    }

    for (basename, identity) in [
        ("aterm", "aterm"),
        ("aterm-cli", "aterm"),
        ("aterm-gui", "aterm-gui"),
        ("aterm-ctl", "aterm-ctl"),
    ] {
        let output = command(app.join("Contents/MacOS").join(basename))
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
    selfcheck_signing(team, &app, &ctx.dmg_path(), &sign::RealAppleTools)
}

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

fn cut_roster_seq(ctx: &CutCtx) -> Result<Option<u64>> {
    if let Some(who) = &ctx.attribution {
        return Ok(Some(who.roster_seq));
    }
    if !ctx.attaches_roster() {
        return Ok(None);
    }
    let text = gates::read_bounded_string(&ctx.manifest_path(), 16 << 20).map_err(|e| {
        Error::new(format!(
            "read {} to learn which machine-roster generation this cut carries: {e}",
            ctx.manifest_path().display()
        ))
    })?;
    Ok(Manifest::parse(&text)
        .map_err(|e| Error::new(format!("staged manifest re-parse failed: {e}")))?
        .roster_seq)
}

/// The roster document this cut publishes: the signing profile's, which `stage_roster_assets`
/// writes into `dist/` only later (a pair left there by an earlier generation never ships), or,
/// for a recovered cut that rebuilt its pair from the published release, that pair.
fn shipped_roster(ctx: &CutCtx) -> Option<std::borrow::Cow<'_, [u8]>> {
    match &ctx.roster {
        Some(document) => Some(std::borrow::Cow::Borrowed(document.bytes.as_slice())),
        None if ctx.attaches_roster() => {
            gates::read_bounded_bytes(&ctx.dist.join(roster::ROSTER_ASSET), 16 << 20)
                .ok()
                .map(std::borrow::Cow::Owned)
        }
        None => None,
    }
}

fn best_published(ctx: &CutCtx) -> Result<Option<u64>> {
    let scanned = verify::scan_published(&ctx.slug, true)?;
    let best = scanned.first();
    let newest_floor = best.and_then(|published| published.min_build);
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
    if let Some(local_roster) = shipped_roster(ctx) {
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
        let origin = native_origin(&ctx.repo)?;
        assert_publisher_session(&origin, guard, fence)?;
        let observed_owner = release_lease_owner(&origin)?;
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
    let origin = native_origin(&ctx.repo)?;
    if let Some(existing) = remote_annotated_tag(&origin, &ctx.tag)? {
        if existing.token != local_token || existing.commit != ctx.commit {
            return Err(Error::new(format!(
                "remote annotated tag {} is already a different immutable object; refusing overwrite",
                ctx.tag
            )));
        }
    } else {
        origin
            .write(&tag_ref, None, &local_token)
            .map_err(Error::new)?;
    }
    let remote = remote_annotated_tag(&origin, &ctx.tag)?.ok_or_else(|| {
        Error::new(format!(
            "remote {} is absent or not annotated after push",
            ctx.tag
        ))
    })?;
    if remote.token != local_token || remote.commit != ctx.commit {
        return Err(Error::new(format!(
            "remote annotated tag {} is not the exact local tag or peels to {}, not claim {}",
            ctx.tag, remote.commit, ctx.commit
        )));
    }
    step("", &format!("tag {} pushed", ctx.tag));
    Ok(())
}

/// Proof that the release channel is reachable, public and writable by this operator's credential.
pub fn preflight_channel_target(slug: &str, claimed: bool) -> Result<()> {
    let repository = crate::github::get(&format!("repos/{slug}")).map_err(|error| {
        Error::new(format!(
            "cannot read the release channel {slug}: {error}. A 404 here means the \
             repository does not exist or this credential cannot see it; create it (public) \
             and grant the release account write access."
        ))
    })?;
    let (Some(private), push) = (
        repository["private"].as_bool(),
        repository["permissions"]["push"].as_bool().unwrap_or(false),
    ) else {
        return Err(Error::new(format!(
            "release channel {slug} returned a repository with no `private` flag"
        )));
    };
    let then = if claimed {
        format!(", then `{CUT_COMMAND}`")
    } else {
        "; nothing was claimed".to_string()
    };
    if !push {
        return Err(Error::new(if crate::github::has_token() {
            format!(
                "the release token ({}) cannot write {slug}: grant it write access{then}",
                channel_token_pointer()
            )
        } else {
            format!(
                "{}, and an anonymous read cannot write {slug}{then}",
                no_channel_token()
            )
        }));
    }
    if private {
        return Err(Error::new(format!(
            "{slug} is private, so no install can read it: make it public{then}"
        )));
    }
    Ok(())
}

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

fn step_publish(ctx: &mut CutCtx) -> Result<()> {
    ensure_ctx_release_lease(ctx)?;
    let slug = ctx.slug.clone();
    preflight_channel_target(&slug, true)?;
    channel::publish_staged_on_channel(&mut LiveChannelRelease {
        ctx,
        slug: &slug,
        bound: None,
    })?;
    draft::forget_published(ctx)?;
    step(
        "publish",
        &format!("v{} (build {}) published on {slug}", ctx.version, ctx.build),
    );
    Ok(())
}

fn prove_roster_on_disk(ctx: &CutCtx, manifest: &Manifest) -> Result<()> {
    prove_roster_on_disk_at(
        ctx,
        manifest,
        aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        roster_now_unix(),
    )
}

fn prove_roster_on_disk_at(
    ctx: &CutCtx,
    manifest: &Manifest,
    master_pubkeys: &[&str],
    now_unix: i64,
) -> Result<()> {
    if !ctx.attaches_roster() {
        if manifest.machine_id.is_some() || manifest.roster_seq.is_some() {
            return Err(Error::new(
                "this cut attaches no machine roster, but its appcast carries machine_id or \
                 roster_seq attribution; refusing to publish an attributed appcast without its \
                 roster pair",
            ));
        }
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
        gates::read_bounded_bytes(&ctx.dist.join(name), 16 << 20)
            .map_err(|e| Error::new(format!("read dist/{name} before publishing it: {e}")))
    };
    let roster_bytes = read(roster::ROSTER_ASSET)?;
    let roster_sig = read(roster::ROSTER_SIG_ASSET)?;
    let parsed = machines::verify_published_roster_parsed(
        master_pubkeys,
        roster_bytes,
        &roster_sig,
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
    parsed.admit(0, now_unix).map_err(|e| {
        Error::new(format!(
            "dist/{} is not a fresh roster at publish time ({e:?}); refusing to publish a \
             lapsed roster onto the channel head",
            roster::ROSTER_ASSET
        ))
    })?;
    let machine = parsed.machine(machine_id, now_unix).map_err(|e| {
        Error::new(format!(
            "dist/{} does not admit machine {machine_id:?} at publish time ({e:?}) — a \
             revocation or expiry rewrote it after this cut was built; refusing to publish an \
             appcast beside a roster that rejects its signer",
            roster::ROSTER_ASSET
        ))
    })?;
    if let Some(expected_pubkey) = ctx.verification_pubkey() {
        let roster_pubkey = canonical_update_pubkey(&machine.pubkey)?;
        let expected_pubkey = canonical_update_pubkey(expected_pubkey)?;
        if roster_pubkey != expected_pubkey {
            return Err(Error::new(format!(
                "dist/{} binds machine {machine_id:?} to public key {roster_pubkey:?}, but \
                 this cut's appcast was signed under {expected_pubkey:?}; refusing to publish \
                 a roster whose machine key disagrees with the appcast signature",
                roster::ROSTER_ASSET
            )));
        }
    }
    Ok(())
}

/// The source signing that attaches the pair to the cut's draft `release_id` without publishing
/// it: the `--keep-draft` form whose `--draft-state` is the cut's own journal, which the bind
/// wrote with that release id, the create intent and the claim commit (a child of the published
/// source), all the engine reads. Unlike the private draft's journal, which a person may have
/// removed or another build's sign rewritten, it names no other draft, and the engine never
/// waits on it for an id. When it does not bind the release (it was edited or lost since), no
/// form is named (`Err`, saying what proceeds instead): the engine would wait on a journal naming
/// no release, and the plain form would publish the draft. This cut then finishes only once that
/// journal binds the release again; else `recover` withdraws it, which, with no cut journal,
/// deletes the draft only while it holds nothing but this version's cut assets
/// ([`withdraw_unpublished_release`]), so a draft carrying any of the source pair is deleted by
/// hand first and recover told `--no-draft-was-posted`.
fn keep_draft_form(ctx: &CutCtx, release_id: u64) -> std::result::Result<String, String> {
    let what = match Journal::load(&ctx.journal_path) {
        Ok(Some(journal))
            if journal.release_id == Some(release_id)
                && journal.release_intent
                && journal.version == ctx.version
                && journal.commit == ctx.commit =>
        {
            return Ok(sign_release_command(
                &ctx.repo,
                &ctx.version,
                Some(&ctx.journal_path),
            ));
        }
        Ok(Some(journal)) => format!(
            "it journals v{} at {} with release {:?} and create intent {}",
            journal.version,
            journal.commit.get(..12).unwrap_or(&journal.commit),
            journal.release_id,
            journal.release_intent
        ),
        Ok(None) => "it is gone".to_string(),
        Err(error) => format!("it cannot be read: {error}"),
    };
    let journal = ctx.journal_path.display();
    Err(format!(
        "no form reaches this draft while the cut's journal {journal} does not bind release \
         {release_id} ({what}): the `--keep-draft` form reads that journal and waits while it \
         names no release, and the plain form would publish the draft. This cut finishes only \
         once {journal} binds release {release_id} again; else withdraw it: with nothing at \
         {journal}, `{SHIP_COMMAND} recover v{version} {commit} --old-publisher-stopped` deletes \
         the draft while it holds nothing but this version's cut assets; one carrying any of the \
         source pair it refuses, keeping the lease, so delete that draft first (`{delete}`) and \
         give recover `--no-draft-was-posted`",
        version = ctx.version,
        commit = ctx.commit,
        delete = draft::delete_command(&ctx.slug, release_id),
    ))
}

fn bind_channel_release(ctx: &mut CutCtx, slug: &str) -> Result<(u64, bool)> {
    let observed = unique_release_object_by_tag(slug, &ctx.tag)?;
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
            if ctx.kind == CutKind::Draft {
                return Err(Error::new(
                    "private uploads require a draft; the source engine must keep the release private until cut",
                ));
            }
            let release = observed.expect("visible release decision");
            if ctx.release_intent {
                step(
                    "publish",
                    &format!("{} on {slug} — resuming this cut's own release", ctx.tag),
                );
            } else {
                adoptable_channel_release(ctx, slug, release.id)?;
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
                 prerelease (its Edit page on github.com: \"Set as a pre-release\"), confirm the \
                 evergreen pointer names the previous app release again, fix the engine side \
                 so the next source release is created as one, then `{CUT_COMMAND}`.",
                tag = ctx.tag
            ))
        },
    )
}

fn release_prerelease_by_id(slug: &str, id: u64) -> Result<bool> {
    let release = crate::github::get(&format!("repos/{slug}/releases/{id}"))?;
    let flag = crate::github::tostring(&release["prerelease"]);
    parse_prerelease_flag(flag.as_str().unwrap_or_default())
        .map_err(|why| Error::new(format!("release ID {id} on {slug}: {why}")))
}

/// `"true"` / `"false"` (one trailing newline allowed), and nothing else.
pub(crate) fn parse_prerelease_flag(text: &str) -> std::result::Result<bool, String> {
    match text.strip_suffix('\n').unwrap_or(text) {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!("invalid prerelease flag {other:?}")),
    }
}

struct LiveChannelRelease<'a> {
    ctx: &'a mut CutCtx,
    slug: &'a str,
    bound: Option<(u64, bool)>,
}

impl LiveChannelRelease<'_> {
    fn bound(&self) -> Result<(u64, bool)> {
        self.bound.ok_or_else(|| {
            Error::new(
                "the channel sequence reached a release call before binding a release; \
                 refusing (channel::publish_staged_on_channel binds right after the floors)",
            )
        })
    }
}

impl channel::ChannelRelease for LiveChannelRelease<'_> {
    fn head_made(&mut self) -> Result<bool> {
        let _timing = crate::cut_timing::Timer::new("channel published state");
        let Some(release_id) = self.ctx.release_id.filter(|_| self.ctx.release_intent) else {
            return Ok(false);
        };
        let (slug, tag) = (self.slug, self.ctx.tag.as_str());
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
        let _manifest = prove_publication_on_disk(ctx)?;
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
        let label = format!(
            "upload {}",
            file.file_name().unwrap_or_default().to_string_lossy()
        );
        let _timing = crate::cut_timing::Timer::new(&label);
        let (release_id, adopted) = self.bound()?;
        upload_channel_asset(
            &std::sync::Mutex::new(&mut *self.ctx),
            self.slug,
            release_id,
            file,
            !adopted,
        )
    }

    fn upload_payloads(&mut self, files: &[PathBuf]) -> Result<()> {
        let (release_id, adopted) = self.bound()?;
        let ctx = std::sync::Mutex::new(&mut *self.ctx);
        let timer = crate::cut_timing::Timer::parallel_parent();
        payload_pool(files, &|file| {
            let label = format!(
                "upload {}",
                file.file_name().unwrap_or_default().to_string_lossy()
            );
            let _timing = timer(&label);
            upload_channel_asset(&ctx, self.slug, release_id, file, !adopted)
        })
    }

    fn prove_assets(&mut self) -> Result<()> {
        let _timing = crate::cut_timing::Timer::new("channel asset proof");
        let (release_id, adopted) = self.bound()?;
        prove_channel_assets(self.ctx, self.slug, release_id, !adopted)
    }

    fn ratchet_head(&mut self) -> Result<()> {
        let _timing = crate::cut_timing::Timer::new("channel head ratchet");
        let (slug, ctx) = (self.slug, &*self.ctx);
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
            &mut head_appcast,
        )
    }

    fn ratchet(&mut self) -> Result<()> {
        self.ratchet_head()?;
        best_published(self.ctx).map(|_| ())
    }

    fn bind(&mut self) -> Result<()> {
        let _timing = crate::cut_timing::Timer::new("bind channel release");
        self.bound = Some(bind_channel_release(self.ctx, self.slug)?);
        Ok(())
    }

    fn make_head(&mut self) -> Result<()> {
        if self.ctx.kind == CutKind::Draft {
            return Err(Error::new("private draft staging cannot publish a release"));
        }
        let _timing = crate::cut_timing::Timer::new("publish full and latest");
        let (release_id, adopted) = self.bound()?;
        let (slug, expect_draft) = (self.slug, !adopted);
        let ctx = &*self.ctx;
        crate::github::send(
            aterm_update_core::github_rest::Method::Patch,
            &format!("repos/{slug}/releases/{release_id}"),
            Some(&channel::head_patch_body()),
            || {
                let release = crate::github::get(&format!("repos/{slug}/releases/{release_id}"))?;
                let current = parse_release_object_response(release.to_string().as_bytes())?;
                validate_channel_release_capability(
                    Some(&current),
                    release_id,
                    &ctx.tag,
                    expect_draft,
                )?;
                let missing = channel::source_attestation_findings(&release);
                if !missing.is_empty() {
                    // The form follows what the release is: the plain form finishes a draft by
                    // publishing it, so a draft is named only the `--keep-draft` form reading the
                    // cut's own journal, which binds exactly this draft; the private draft's
                    // journal may be gone or another build's by now. The finding stands whatever
                    // the journal read says.
                    let form = if expect_draft {
                        keep_draft_form(ctx, release_id)
                    } else {
                        Ok(sign_release_command(&ctx.repo, &ctx.version, None))
                    };
                    let remedy = match form {
                        Ok(form) => {
                            format!("finish the source signing ({form}), then resume the cut")
                        }
                        Err(none) => none,
                    };
                    return Err(Error::new(format!(
                        "release ID {release_id} lacks a unique signed source manifest: {}; \
                         {remedy}",
                        channel::attestation_words(slug, &missing),
                    )));
                }
                ensure_ctx_release_lease(ctx)?;
                Ok(())
            },
        )?;
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
        let _timing = crate::cut_timing::Timer::new("public head and anonymous downloads");
        let (release_id, _) = self.bound()?;
        let slug = self.slug;
        let ctx = &*self.ctx;
        let after = release_object_by_id(slug, release_id)?;
        validate_channel_release_capability(after.as_ref(), release_id, &ctx.tag, false)?;
        // All three read the published release after its head PATCH. None
        // establishes a fact another consumes; all must pass before returning.
        let timer = crate::cut_timing::Timer::parallel_parent();
        upload_pool(&[0, 1, 2], 3, &|probe| {
            let label = match probe {
                0 => "public client election",
                1 => "evergreen pointer propagation",
                _ => "anonymous asset HEADs",
            };
            let _timing = timer(label);
            match probe {
                0 => prove_client_elects_this_cut(ctx, slug, release_id),
                1 => prove_evergreen_pointer_serves_this_cut(ctx, slug),
                _ => prove_channel_assets_download_anonymously(ctx, slug),
            }
        })
    }
}

/// What one credential-free, redirect-refusing HEAD of a release asset's download URL says about whether a stranger can download it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetHead {
    Downloads,
    NotServed,
    Refused(u16),
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

/// The readability proof. Four independent HEADs may run together; every
/// admitted request settles before a refusal returns, without application retries.
pub fn prove_assets_download_anonymously(
    slug: &str,
    tag: &str,
    names: &[String],
    head: &(
         dyn Fn(
        &str,
    )
        -> std::result::Result<aterm_update_core::HeadAnswer, aterm_update_core::HttpError>
             + Sync
     ),
) -> Result<u32> {
    let (owner, repo) = slug
        .split_once('/')
        .ok_or_else(|| Error::new(format!("not an owner/repo slug: {slug}")))?;
    let issued = std::sync::atomic::AtomicU32::new(0);
    let timer = crate::cut_timing::Timer::parallel_parent();
    upload_pool(names, 4, &|name| {
        let _timing = timer(&format!("anonymous HEAD {name}"));
        let url = aterm_update_core::cdn::release_download_url(owner, repo, tag, name).ok_or_else(
            || {
                Error::new(format!(
                    "no download URL can be derived for {name:?} of {tag} in {slug}"
                ))
            },
        )?;
        if aterm_update_core::cdn::is_api_host(&url) {
            return Err(Error::new(format!(
                "refusing to probe {url}: the readability proof never spends the metered \
                 anonymous API"
            )));
        }
        issued.fetch_add(1, Ordering::Relaxed);
        match classify_asset_head(head(&url)) {
            AssetHead::Downloads => {}
            AssetHead::NotServed => {
                return Err(Error::new(format!(
                    "the public channel {slug} does not serve {name} to a client with no \
                     credential: an unauthenticated HEAD of {url} answered 404. That is a final \
                     absence answer, not a transient transport failure. The authenticated checks \
                     passed, so the repository is private (GitHub answers 404 on this host for \
                     one) or the upload never completed. Make {slug} public, then `{CUT_COMMAND}`.",
                )));
            }
            AssetHead::Inconclusive(detail) => {
                return Err(Error::new(format!(
                    "cannot prove a credential-less client can download {name} from {slug}: \
                     the unauthenticated HEAD of {url} answered {detail} after its own bounded \
                     transport retries — not a redirect into GitHub's release-asset storage. \
                     Check the network, then `{CUT_COMMAND}`."
                )));
            }
            AssetHead::Refused(code) => {
                return Err(Error::new(format!(
                    "GitHub's download host answered HTTP {code} to an unauthenticated HEAD of \
                     {url} — not the API budget (that host has none): a 403 is a proxy, a \
                     firewall or an abuse block, a 429 is GitHub throttling this address. Asked \
                     once; it says nothing about whether {slug} is public. Check from another \
                     network (`curl -sI {url}` answers 302 when the asset is served), then \
                     `{CUT_COMMAND}` (re-uploads nothing)."
                )));
            }
        }
        Ok(())
    })?;
    Ok(issued.load(Ordering::Relaxed))
}

/// Attempts for the evergreen pointer to name this cut once its head PATCH is sent ([`prove_pointer_serves`]).
pub const POINTER_PROBE_ATTEMPTS: u32 = 30;

/// Gap between evergreen-pointer attempts.
pub const POINTER_PROBE_DELAY: Duration = Duration::from_secs(10);

/// What the evergreen pointer (`https://github.com/<slug>/releases/latest/download/<asset>`) answers a credential-less client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PointerProbe {
    Tag { tag: String, location: String },
    NoRelease,
    Refused { code: u16, reason: String },
    NotAnApp { tag: String },
    Other(String),
}

/// Run one anonymous probe of the evergreen pointer.
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
        Err(PointerError::OtherTag { tag }) => Ok(PointerProbe::NotAnApp { tag }),
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

/// The pointer gate.
pub fn prove_pointer_serves(
    slug: &str,
    tag: &str,
    local_manifest: &[u8],
    probe: &mut dyn FnMut() -> Result<PointerProbe>,
    fetch: &mut dyn FnMut(&str) -> Result<Vec<u8>>,
    sleep: &mut dyn FnMut(Duration),
) -> Result<Manifest> {
    let asset = manifest_out::MANIFEST_ASSET;
    // GitHub recomputes `latest` after the head PATCH: for a while the pointer
    // still answers, correctly and finally for that request, with the previous
    // release. That is propagation, not a transport failure, so the shared
    // transport retry never sees it and this gate waits within its own bound.
    let mut last = PointerProbe::NoRelease;
    let mut location = None;
    for attempt in 1..=POINTER_PROBE_ATTEMPTS {
        last = crate::cut_timing::measure("evergreen pointer request", &mut *probe)?;
        if let PointerProbe::Tag {
            tag: named,
            location: loc,
        } = &last
            && named == tag
        {
            location = Some(loc.clone());
            break;
        }
        if matches!(last, PointerProbe::Refused { .. }) {
            break;
        }
        if attempt < POINTER_PROBE_ATTEMPTS {
            crate::cut_timing::measure("evergreen propagation wait", || sleep(POINTER_PROBE_DELAY));
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

fn prove_evergreen_pointer_serves_this_cut(ctx: &CutCtx, slug: &str) -> Result<()> {
    let asset = manifest_out::MANIFEST_ASSET;
    let local = gates::read_bounded_bytes(&ctx.manifest_path(), 16 << 20).map_err(|error| {
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

fn anonymous_get_optional(location: &str) -> Result<Option<Vec<u8>>> {
    match aterm_update_core::download_bytes(location, 16 << 20) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if aterm_update_core::download_error_is_not_found(&error) => Ok(None),
        Err(error) => Err(Error::new(format!(
            "an unauthenticated GET of {location} failed ({error})"
        ))),
    }
}

/// The head's appcast at `location` (the tag URL the evergreen pointer named), through the cut's
/// release reader ([`crate::github::reader`]): the web route, then the API route when the
/// download host fails, so a passing outage does not refuse the cut; `None` on its absence.
fn head_appcast(location: &str) -> Result<Option<Vec<u8>>> {
    let reader = crate::github::reader(crate::machines::CUT_READ_PATIENCE)?;
    match reader.bytes_at(location, 16 << 20) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(aterm_update_core::release_asset::AssetError::Absent(_)) => Ok(None),
        Err(error) => Err(Error::new(format!("a read of {location} failed ({error})"))),
    }
}

fn anonymous_get(location: &str) -> Result<Vec<u8>> {
    anonymous_get_optional(location)?
        .ok_or_else(|| Error::new(format!("an unauthenticated GET of {location} answered 404")))
}

/// The head ratchet.
pub fn prove_channel_head_is_older(
    slug: &str,
    tag: &str,
    build: u64,
    probe: &mut dyn FnMut() -> Result<PointerProbe>,
    fetch: &mut dyn FnMut(&str) -> Result<Option<Vec<u8>>>,
) -> Result<()> {
    let (head, location) = match probe()? {
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

fn prove_channel_assets_download_anonymously(ctx: &CutCtx, slug: &str) -> Result<()> {
    let names = ctx.channel_asset_names();
    let issued = prove_assets_download_anonymously(
        slug,
        &ctx.tag,
        &names,
        &aterm_update_core::head_no_redirect,
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

fn create_channel_draft(ctx: &mut CutCtx, slug: &str) -> Result<ReleaseObjectIdentity> {
    let notes = release_body_for_post("publish", &ctx.notes_path())?;
    let title = format!("aterm {}", ctx.version);
    let payload = release_create_payload(ctx.tag.as_str(), &title, &notes)?;
    let post = OneShotPost::prepare_json(&format!("repos/{slug}/releases"), &payload)?;
    ensure_ctx_release_lease(ctx)?;
    let permit = ctx.persist_release_intent()?;
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
    if server_refused_without_creating(&out) {
        ctx.release_release_intent()?;
        return Err(Error::new(format!(
            "release create for {} was REFUSED by {slug} ({}); nothing was created, so the \
             release intent has been released and `--resume` may post once more.",
            ctx.tag,
            release_body_post_failure_detail(&out, &notes)
        )));
    }
    Err(Error::new(format!(
        "release create returned failure and no exact release object is visible for {} on \
         {slug}; refusing an ambiguous retry in this invocation (resume after GitHub \
         converges): {}",
        ctx.tag,
        release_body_post_failure_detail(&out, &notes)
    )))
}

/// Request-wall throughput includes connection setup and the server's response;
/// a failed request does not establish how many payload bytes reached GitHub.
fn upload_observation(name: &str, bytes: u64, wall: f64, success: bool) -> aterm_json::Value {
    aterm_json::json!({
        "schema": 1,
        "event": "cut-upload-transfer",
        "asset": name,
        "payload_bytes": bytes,
        "execution_s": wall,
        "confirmed_bytes": success.then_some(bytes),
        "bytes_per_request_second": (success && wall > 0.0).then_some(bytes as f64 / wall),
        "success": success,
    })
}

/// Stop dispatch on failure, but let in-flight one-shot POSTs settle their
/// durable intents. Cancelling those requests would create ambiguous outcomes.
pub(crate) fn upload_pool<T: Sync>(
    files: &[T],
    bound: usize,
    upload: &(impl Fn(&T) -> Result<()> + Sync),
) -> Result<()> {
    let queue = std::sync::Mutex::new((0, Vec::new()));
    #[cfg(test)]
    let fixture = crate::github::Fixture::current();
    std::thread::scope(|scope| {
        for _ in 0..bound.max(1).min(files.len()) {
            let queue = &queue;
            #[cfg(test)]
            let fixture = fixture.clone();
            scope.spawn(move || {
                #[cfg(test)]
                let _route = fixture.map(crate::github::Fixture::enter);
                loop {
                    let index = {
                        let mut queue = queue.lock().unwrap();
                        if !queue.1.is_empty() || queue.0 == files.len() {
                            break;
                        }
                        let index = queue.0;
                        queue.0 += 1;
                        index
                    };
                    if let Err(error) = upload(&files[index]) {
                        queue.lock().unwrap().1.push((index, error));
                    }
                }
            });
        }
    });
    let (_, mut errors) = queue.into_inner().unwrap();
    errors.sort_by_key(|(index, _)| *index);
    errors
        .into_iter()
        .next()
        .map_or(Ok(()), |(_, error)| Err(error))
}

/// The channel's validated payloads share the measured request pool. Each
/// one-shot POST retains its durable intent; the manifest barrier stays in channel.
fn payload_pool<T: Sync>(files: &[T], upload: &(impl Fn(&T) -> Result<()> + Sync)) -> Result<()> {
    // Equal-volume private GitHub upload ABBA: 2=73.55s, 4=45.33s,
    // 8=23.78s (128 MiB). See docs/measured/release-draft-tail-2026-11-09.md.
    upload_pool(files, 8, upload)
}

#[cfg(test)]
#[path = "publish_upload_pool_tests.rs"]
mod upload_pool_tests;

pub(crate) fn upload_channel_asset(
    ctx: &std::sync::Mutex<&mut CutCtx>,
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
    let prior_intent = ctx.lock().unwrap().upload_intent_issued(name);
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
                    ensure_upload_release_lease(ctx)?;
                    let endpoint = format!("repos/{slug}/releases/assets/{stale_id}");
                    if let Err(error) = crate::github::send_once(
                        aterm_update_core::github_rest::Method::Delete,
                        &endpoint,
                        None,
                    ) {
                        return Err(Error::new(format!(
                            "failed to delete stale {name} (asset ID {stale_id}) on {slug}: {error}"
                        )));
                    }
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
    let bytes = fs::metadata(file)
        .map_err(|error| Error::new(format!("read upload size for {name}: {error}")))?
        .len();
    let post = OneShotPost::prepare_binary(&endpoint, file)?;
    let release = release_object_by_id(slug, release_id)?;
    ensure_upload_release_lease(ctx)?;
    let permit = {
        let mut ctx = ctx.lock().unwrap();
        validate_channel_release_capability(release.as_ref(), release_id, &ctx.tag, expect_draft)?;
        ctx.persist_upload_intent(name)?
    };
    let started = std::time::Instant::now();
    let out = crate::cut_timing::measure(&format!("upload POST {name}"), || post.issue(permit));
    println!(
        "release-event: {}",
        upload_observation(
            name,
            bytes,
            started.elapsed().as_secs_f64(),
            out.as_ref().is_ok_and(RunOut::success)
        )
    );
    let out = out?;
    let visible = || {
        release_asset_identity_for_release_id_optional(slug, release_id, name)
            .map(|found| found.is_some())
    };
    match after_upload_post(
        || ctx.lock().unwrap().settle_upload_intent(name, &out),
        visible,
    )? {
        AfterUploadPost::Converge { journal } => {
            verify_release_asset_id_matches_local(slug, release_id, name, file)?;
            if journal {
                step(
                    "publish",
                    &format!(
                        "upload of {name} was refused ({}) but the asset is on {slug} with this \
                         cut's bytes; converging on it and journaling it",
                        out.stderr_utf8().trim()
                    ),
                );
                ctx.lock().unwrap().record_upload_intent(name)?;
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
    prove_asset_files(
        &ctx.channel_asset_paths(),
        &inventory_before,
        slug,
        release_id,
    )?;
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

fn prove_asset_files(
    files: &[PathBuf],
    inventory: &[ReleaseAssetInventoryEntry],
    slug: &str,
    release_id: u64,
) -> Result<()> {
    let timer = crate::cut_timing::Timer::parallel_parent();
    upload_pool(files, 4, &|file| {
        let _timing = timer("asset byte proof");
        let name = file
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| Error::new("release artifact filename is not UTF-8"))?;
        let asset = inventory
            .iter()
            .find(|asset| asset.name == name)
            .ok_or_else(|| Error::new(format!("release ID {release_id} has no asset {name}")))?;
        verify_release_asset_bytes(slug, release_id, name, file, (asset.id, asset.size)).map(|_| ())
    })
}

#[cfg(test)]
#[path = "publish_asset_batch_tests.rs"]
mod asset_batch_tests;

fn prove_client_elects_this_cut(ctx: &CutCtx, slug: &str, release_id: u64) -> Result<()> {
    let live = release_object_by_id(slug, release_id)?;
    validate_channel_release_capability(live.as_ref(), release_id, &ctx.tag, false)?;
    let names: Vec<String> = release_asset_inventory_for_release_id(slug, release_id)?
        .into_iter()
        .map(|asset| asset.name)
        .collect();
    ctx.validate_channel_assets(&names)?;

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
    let local_manifest =
        gates::read_bounded_string(&ctx.manifest_path(), 16 << 20).map_err(|error| {
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

/// The canonical upload path of asset `name` on release `release_id` of `slug`.
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
    Ok(format!(
        "repos/{slug}/releases/{release_id}/assets?name={name}"
    ))
}

#[cfg(test)]
#[path = "publish_release_body_post_tests.rs"]
mod release_body_post_tests;

#[cfg(test)]
#[path = "publish_transport_body_tests.rs"]
mod transport_body_tests;

#[cfg(test)]
#[path = "publish_roster_wiring_tests.rs"]
mod roster_wiring_tests;

#[cfg(test)]
#[path = "publish_lean_dmg_ceiling_tests.rs"]
mod lean_dmg_ceiling_tests;

#[cfg(test)]
#[path = "publish_anonymous_readability_tests.rs"]
mod anonymous_readability_tests;

#[cfg(test)]
#[path = "publish_remedy_tests.rs"]
mod remedy_tests;

#[cfg(test)]
#[path = "publish_artifact_proof_tests.rs"]
mod artifact_proof_tests;

#[cfg(test)]
#[path = "publish_local_probe_tests.rs"]
mod local_probe_tests;

#[cfg(test)]
#[path = "publish_signed_copy_tests.rs"]
mod signed_copy_tests;
