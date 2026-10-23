// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Release publication pipeline, GitHub draft/release management, and asset upload verification.
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
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

/// Every `cut` flag (spec §5), parsed by cli.rs.
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
}

/// Which cut flavor is running — decided once, checked per step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutKind {
    Real,
    DryRun,
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
        .and_then(|v| v.trim().parse::<usize>().ok())
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
        let hang = " ".repeat(lead + marker.chars().count());
        let mut line = String::from(&seg[..lead]);
        line.push_str(marker);
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
    debug_assert!(
        label.chars().count() <= LABEL_MAX,
        "label {label:?} is {} columns; the grid carries {LABEL_MAX} \
         (a longer one eats its own separator — see VALUE_COL)",
        label.chars().count()
    );
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

/// One transcript line (or block).
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

/// Read the source tree's `[workspace.package]` `MAJOR.MINOR.0` version.
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

/// The version a cut publishes.
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

/// The next release version after `release`.
pub fn bump_minor_release(release: &str) -> Result<String> {
    let (major, minor, _patch) = version_components(release)?;
    let minor = minor.checked_add(1).ok_or_else(|| {
        Error::new(format!(
            "version {release:?} cannot bump MINOR without overflow"
        ))
    })?;
    Ok(format!("{major}.{minor}.0"))
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
            let slug = tail.trim_end_matches('/').trim_end_matches(".git");
            if slug.split('/').count() == 2 {
                return Some(slug.to_string());
            }
        }
    }
    None
}

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
    let cargo_text = fs::read_to_string(&path)
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

/// The release-org token on disk.
pub(crate) fn channel_token_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".secrets/gh_access_token_alabsystems"))
}

/// Dedicated cooperative lock for every real release cut.
pub const RELEASE_LEASE_REF: &str = "refs/tags/aterm-release-lease";

/// Per-invocation fencing token.
pub const PUBLISHER_FENCE_REF: &str = "refs/tags/aterm-release-fence";

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

/// Read the unique annotated publisher fence and its peeled claim.
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

/// Version of the `fence-*` identity block in the fence tag body.
pub const FENCE_IDENTITY_FORMAT: u32 = 2;

static FENCE_SELF_VERSION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Record what this invocation publishes.
pub fn set_publisher_fence_version(version: &str) {
    if let Ok(mut slot) = FENCE_SELF_VERSION.lock() {
        *slot = Some(version.trim().trim_start_matches('v').to_string());
    }
}

fn publisher_fence_self_version() -> Option<String> {
    FENCE_SELF_VERSION.lock().ok().and_then(|slot| slot.clone())
}

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
    fn boot(&self) -> Option<String>;
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
    fn uname_n() -> Option<String> {
        let out = Command::new("uname").arg("-n").output().ok()?;
        if !out.status.success() {
            return None;
        }
        let host = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!host.is_empty()).then_some(host)
    }

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

    fn pgrp_from_stat(stat: &str) -> Option<u32> {
        let (_, after) = stat.rsplit_once(')')?;
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
                    fs::read_to_string(dir.join("comm"))
                        .ok()
                        .map(|name| name.trim().to_string())
                });
            return PidState::Running { exe };
        }
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
                    (Some(1), true) => PidState::NotRunning,
                    (code, _) => PidState::Unknown(format!(
                        "ps -p {pid} exited {}: {complaint}",
                        code.map_or_else(|| "by signal".to_string(), |code| code.to_string())
                    )),
                }
            }
        }
    }
}

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
    Alive(String),
    Dead(String),
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

/// Read the identity block out of the fence's annotated-tag object.
pub fn publisher_fence_identity(git: &dyn GitRunner, token: &str) -> FenceIdentity {
    if !valid_lease_owner(token) {
        return FenceIdentity::unreadable(format!("malformed fence token {token:?}"));
    }
    let local = git
        .git(&["cat-file", "-t", token])
        .map(|out| out.success() && out.stdout_utf8().trim() == "tag")
        .unwrap_or(false);
    if !local {
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
    NoFence,
    Reclaimed { token: String, detail: String },
    Kept { refusal: String },
}

/// `--resume`'s automatic reclaim.
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

/// Read-only fresh-cut preflight.
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

/// The cut's pre-claim gates that need nothing `pub publish` makes.
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
    git: &dyn GitRunner,
    expected_owner: &str,
) -> Result<ReleaseLeaseGuard> {
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

/// Create a unique per-process fence.
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

/// Explicit killed-machine takeover.
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

/// Re-prove both persistent claim ownership and the exact process token before each release mutation.
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

/// Delete only this process's exact token.
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

/// AN UNPUBLISHED CUT'S LAST TWO WRITES, in their one order.
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

/// Final unlock deletes the persistent owner and the process token in one atomic ref transaction.
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
    if now.as_deref() != Some(expected_owner.as_str()) {
        return Ok(LeaseRelease::AlreadySuperseded);
    }
    Err(Error::new(format!(
        "CAS release of {RELEASE_LEASE_REF} failed: {}; current owner is {}",
        out.stderr_utf8().trim(),
        now.as_deref().unwrap_or("absent")
    )))
}

/// Replay unlock-only cleanup for a completed session when no fence guard is held.
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

fn read_journal_text(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::new(format!("read {}: {e}", path.display()))),
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
    #[serde(default)]
    pub done: Vec<String>,
}

impl Journal {
    /// Read this cutter's journal.
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

    /// This cutter's journal, or `Ok(None)` for no journal and for another cutter's format.
    pub fn load_if_ours(path: &Path) -> Result<Option<Journal>> {
        match JournalHeader::read(path)? {
            Some(header) if header.format == JOURNAL_FORMAT => Self::load(path),
            _ => Ok(None),
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

/// A fully prepared one-shot POST ([`crate::github::prepare`]): every fallible preflight is done before the caller persists its durable intent, and the token rides curl's stdin (`-H @-`), never its argv, environment or a file.
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
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
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
    aterm_json::to_vec(&aterm_json::json!({
        "tag_name": tag,
        "name": title,
        "body": &**body,
        "draft": true,
        "prerelease": false,
    }))
    .map_err(|error| Error::new(format!("serialize release request: {error}")))
}

impl OneShotPost {
    /// JSON-body POST to `path` on the API origin (the channel draft's create). A draft
    /// body is a few hundred bytes, written to a private scratch file curl reads.
    fn prepare_json(path: &str, payload: &[u8]) -> Result<Self> {
        crate::github::prepare(
            crate::github::OneShotUrl::Api(path),
            aterm_update_core::github_rest::Body::Json(payload),
        )
        .map(Self)
    }

    /// Raw file-body POST to `path` on the uploads origin (an asset upload), streamed by `--upload-file`, never buffered: the DMG is over a gigabyte.
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

/// One appcast-listing row of `release`, as [`parse_appcast_asset_listing`] reads it: its id, tag, draft flag, target and its exact appcast pair's ids and names.
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
        .map(|a| aterm_json::json!({"id": a["id"].clone(), "name": a["name"].clone()}))
        .collect();
    aterm_json::json!({
        "release_id": release["id"].clone(),
        "tag": release["tag_name"].clone(),
        "draft": release["draft"].clone(),
        "target_commitish": release["target_commitish"].clone(),
        "assets": assets,
    })
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

/// Releases per listing page: GitHub's most.
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
             key: cut with --release-credentials ~/.aterm/release-credentials.toml (`targo \
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
        // and `Inactive` makes every hook a no-op if one were somehow reached.
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
    // A 404 is an answer (the object is absent), never retried.
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
    // A 404 is an answer, never retried (`release_object_by_id`'s rule); any other failure retries.
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
        // symbolic release target into a different mutation capability.
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

/// Read at most `limit + 1` bytes so a metadata/download replacement race is still memory-bounded.
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

/// Concurrently drain a child's diagnostic stream to EOF while retaining only a bounded prefix.
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

/// The bytes of asset `id` on `slug`, streaming ([`crate::github::open_asset`]): the asset
/// API's redirect, held to the asset CDN's origins (any other `Location` is refused and never
/// asked), then an anonymous curl of the CDN with its stdout and stderr piped.
fn exact_release_asset_download(slug: &str, id: u64) -> Result<aterm_uds::spawnfd::Child> {
    exact_release_asset_child(slug, id).map_err(|error| {
        Error::new(format!(
            "exact GitHub asset-ID {id} download on {slug} could not start: {error}"
        ))
    })
}

/// [`exact_release_asset_download`], its failure as the REST client classed it.
fn exact_release_asset_child(
    slug: &str,
    id: u64,
) -> std::result::Result<aterm_uds::spawnfd::Child, aterm_update_core::github_rest::RestError> {
    crate::github::open_asset(slug, id)
}

// A lost body (curl: "transfer closed with N bytes remaining to read") and a server that
// answered "no" both end the CDN curl non-zero. A 404, a 401 and a permission refusal are
// answers; only a transport failure can come out differently the second time.

/// Whether repeating a bounded asset transfer can possibly change its outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AssetTransferFault {
    Transport,
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

/// Sleeps between attempts (so one more attempt than entries), the same 2s/5s shape as [`crate::github::retried`].
pub(crate) const ASSET_TRANSFER_BACKOFFS: &[u64] = &[2, 5];

/// Decide, from a diagnostic (the CDN curl's stderr, or the REST client's message), whether the transport failed or the server answered; unrecognised is `Answered`.
#[must_use]
pub(crate) fn classify_asset_transfer_failure(diagnostic: &str) -> AssetTransferFault {
    let d = diagnostic.to_ascii_lowercase();
    const ANSWERED: &[&str] = &[
        // The REST client's words for GitHub's answer (`HTTP 404: Not Found`).
        "http 401",
        "http 403",
        "http 404",
        "http 410",
        "http 422",
        // curl's `--fail` words for the CDN's (`The requested URL returned error: 404`).
        "returned error: 401",
        "returned error: 403",
        "returned error: 404",
        "returned error: 410",
        "not found",
        "bad credentials",
        "requires authentication",
        "resource not accessible",
        "must have admin rights",
        "forbidden",
        "permission",
        "no such release",
        "saml enforcement",
        "refused, never asked",
        "maximum file size exceeded",
    ];
    const TRANSPORT: &[&str] = &[
        // curl read a body that stopped short.
        "transfer closed with",
        "partial file",
        "unexpected eof",
        "unexpected end of stream",
        "recv failure",
        "send failure",
        "empty reply from server",
        "connection reset",
        "connection refused",
        "connection closed",
        "failed to connect",
        "broken pipe",
        "operation timed out",
        "operation too slow",
        "timed out",
        "timeout",
        "ssl connect error",
        "tls handshake",
        "handshake failure",
        "could not resolve host",
        "temporary failure in name resolution",
        "network is unreachable",
        "network is down",
        "http/2 stream",
        "http 500",
        "http 502",
        "http 503",
        "http 504",
        "returned error: 500",
        "returned error: 502",
        "returned error: 503",
        "returned error: 504",
        "curl reached no answer",
        "server error",
    ];
    if ANSWERED.iter().any(|m| d.contains(m)) {
        return AssetTransferFault::Answered;
    }
    if TRANSPORT.iter().any(|m| d.contains(m)) {
        return AssetTransferFault::Transport;
    }
    AssetTransferFault::Answered
}

/// The bounded retry, with the transfer injected so the loop is testable without a network.
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

fn attempt_exact_release_asset_transfer(
    slug: &str,
    id: u64,
    expected_size: u64,
    limit: usize,
) -> std::result::Result<Vec<u8>, TransferAttemptFailure> {
    // The asset API's own failure is classed as the client read it: a definitive answer is
    // not asked again, a transport failure or a 5xx is.
    let mut child = exact_release_asset_child(slug, id).map_err(|error| {
        let message = format!("exact GitHub asset-ID {id} download on {slug}: {error}");
        if crate::github::definitive(&error) {
            TransferAttemptFailure::answered(message)
        } else {
            TransferAttemptFailure::transport(message)
        }
    })?;
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

    use super::*;
    use std::cell::RefCell;

    /// The CDN curl's stderr for a truncated body.
    const TRUNCATED: &str = "curl: (18) transfer closed with 1048576 bytes remaining to read";

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
            "HTTP 404: Not Found",
            "HTTP 403: Resource not accessible by integration",
            "HTTP 401: Bad credentials",
            "curl: (22) The requested URL returned error: 404",
            "the asset API redirected off the asset CDN, to https://example.com; refused, never \
             asked",
        ] {
            assert_eq!(
                classify_asset_transfer_failure(answered),
                AssetTransferFault::Answered,
                "{answered}"
            );
        }
        for transport in [
            "curl: (56) Recv failure: Connection reset by peer",
            "curl: (28) SSL connection timeout",
            "curl: (6) Could not resolve host: release-assets.githubusercontent.com",
            "HTTP 502: Server Error",
            "curl: (22) The requested URL returned error: 503",
            "curl reached no answer (exit 7): curl: (7) Failed to connect to api.github.com",
        ] {
            assert_eq!(
                classify_asset_transfer_failure(transport),
                AssetTransferFault::Transport,
                "{transport}"
            );
        }
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
            "curl: (22) The requested URL returned error: 404",
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

/// The bracketed transfer.
pub(crate) fn download_release_asset_with_identity_and_recheck(
    slug: &str,
    name: &str,
    before: (u64, u64),
    mut recheck: impl FnMut() -> Result<(u64, u64)>,
) -> Result<Vec<u8>> {
    let limit = validate_small_release_asset_size(name, before.1)?;
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

/// Copy and hash an asset without ever writing more than `limit` bytes.
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

const fn roster_duty(build_done: bool) -> RosterDuty {
    if build_done {
        RosterDuty::Finish
    } else {
        RosterDuty::Sign
    }
}

/// The anchors and ambient inputs [`signing_verdict`] resolves against.
pub struct SigningAnchors<'a> {
    pub master_pubkeys: &'a [&'a str],
    pub identity_path: Option<&'a Path>,
    pub now_unix: i64,
    pub duty: RosterDuty,
}

pub fn signing_verdict(
    creds: Option<&sign::ReleaseCredentials>,
    anchors: &SigningAnchors<'_>,
) -> Result<SigningVerdict> {
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

/// Assemble the manifest, stamp the attribution into it, prove the bytes and write them, in that order.
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

/// The monotonic gate (spec §7 steps 4+5).
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
    pub notes_section: String,
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

    fn attaches_roster(&self) -> bool {
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

/// Bind a published manifest's commit identity to the exact annotated git tag the release advertises.
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

/// Delete an exact annotated tag token while an injected safety proof remains true.
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

/// One DELETE of `endpoint`, asked once: what GitHub or curl said when it failed, else
/// nothing. The caller re-reads the object to learn whether it is gone, so a failure is
/// only words here.
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

fn recovery_claim_build(git: &dyn GitRunner, version: &str, owner: &str) -> Result<u64> {
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
    let cargo_text = fs::read_to_string(repo.join("Cargo.toml"))
        .map_err(|error| Error::new(format!("read Cargo.toml: {error}")))?;
    let origin_slug = repo_slug(&cargo_text)
        .ok_or_else(|| Error::new("Cargo.toml repository is not an exact GitHub OWNER/REPO URL"))?;
    let slug = channel_slug(&cargo_text)?;
    let git = GitCli::new(repo);
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
    let fence = rotate_publisher_fence_for_recovery(&git, &owner)?;
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
        artifact: None,
        macho_sha256: None,
        linux: None,
        manifest_signed: signature_policy.required,
        signature_required: signature_policy.required,
        signature_pubkey: signature_policy.pubkey.clone(),
        verify_pubkey: recovered_pubkey.clone(),
        signature_machine_id: manifest.machine_id.clone(),
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
        notes_section: version.to_string(),
        journal: Some(journal),
    };
    run_pipeline(&mut ctx, Instant::now())
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
        CutKind::Real => Err(Error::new(format!(
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

/// THE VERIFIED BUNDLE OF THE RELEASE OF `source`, read from this clone's release journal (`ship release`'s, in the git common dir).
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

/// The whole `targo --unverified ship cut` (spec §7 order).
pub fn run_cut(repo: &Path, opts: &CutOptions) -> Result<()> {
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
        // it and move on.
        let _ = fs::remove_file(&journal_path);
    }
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
        // before the placement was this checkout's.
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
    let apple = resolve_apple_tier(aterm_update_core::pins::APPLE_TEAM_ID, credentials)?;

    let changelog_text = fs::read_to_string(tree.join(changelog::CHANGELOG_FILE))
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

    let gate_opts = gates::GateOpts {
        version: version.clone(),
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
    let verified = verified_release_bundle(&operator, repo, &source, &version, &toolchain)?;
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
    let fixtures = gates::handoff_fixture_gate_at(&git, &source, &version)?;
    step("gate", &gates::handoff_fixtures_line(fixtures.as_ref()));
    let policy = gates::handoff_policy_gate_at(&git, &source, &version)?;
    step(
        "gate",
        &format!("handoff policy sealed into the bundle: {policy}"),
    );
    step("gate", &verified.line);
    if kind != CutKind::Real && !opts.linux_workers.is_empty() {
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
        preflight_channel_target(&publish_slug, false)?;
        step(
            "channel",
            &format!("release channel {publish_slug} is public and writable (pre-claim)"),
        );
    }

    let head_scan = verify::scan_head_before_claim(&publish_slug)?;
    let newest_channel = head_scan.first();
    let newest_min_build = newest_channel.and_then(|published| published.min_build);
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
    let ledger_text = if kind == CutKind::Real {
        ledger::show_origin_ledger(&git)?
    } else {
        fs::read_to_string(tree.join(ledger::LEDGER_FILE))
            .map_err(|e| Error::new(format!("read {}: {e}", ledger::LEDGER_FILE)))?
    };
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
    set_publisher_fence_version(&journal.version);
    println!(
        "aterm-release · cut v{} (build {}) — RESUME at step \"{next}\"",
        journal.version, journal.build_number
    );

    ordinary_resume_claim_preflight(tree, &git, &journal)?;

    if recovered_session.is_none() && next != "unlock" {
        match reclaim_dead_publisher_fence(&git, &journal.commit, &LocalProbe) {
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
    };
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

fn revalidate_ctx_signature_policy(ctx: &CutCtx) -> Result<()> {
    if ctx.kind != CutKind::Real {
        return Ok(());
    }
    ensure_ctx_release_lease(ctx)?;
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
    if duty == RosterDuty::Sign {
        resume_attribution_agrees(
            ctx.signature_machine_id.as_deref(),
            observed.machine_id().as_deref(),
        )?;
    }
    ensure_ctx_release_lease(ctx)
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

/// The two container builders and the two digest reads, behind a trait.
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
        (pack.sha256(&dmg_out.path)?, pack.size(&dmg_out.path)?)
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

fn step_sign(ctx: &mut CutCtx) -> Result<()> {
    if let Some(handoff) = &mut ctx.linux {
        handoff.run_workers(&ctx.version, ctx.build, &ctx.commit)?;
        handoff.import(&ctx.dist, &ctx.version, ctx.build, &ctx.commit)?;
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
    let provenance_path = bundle::write_provenance(&spec, &app, &signed_by, &macho_sha256)?;
    if ctx.signature_required {
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
    let cl_text = fs::read_to_string(ctx.tree.join(changelog::CHANGELOG_FILE))
        .map_err(|e| Error::new(format!("read {}: {e}", changelog::CHANGELOG_FILE)))?;
    let body = changelog::rolled_body(&cl_text, &ctx.notes_section)?;
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
        zip_sha256: &zout.sha256,
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
    stage_roster_assets(&ctx.dist, ctx.roster.as_ref())?;
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
/// The signing half of the self-check.
pub fn selfcheck_signing(
    team: &str,
    app: &Path,
    dmg: &Path,
    tools: &dyn sign::AppleTools,
) -> Result<&'static str> {
    tools.codesign_verify_strict(app).map_err(|e| {
        Error::new(format!(
            "self-check failed: codesign --verify --deep --strict: {e}"
        ))
    })?;
    if team.is_empty() {
        // keep: no Apple tool is spawned.
        return Ok("");
    }
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

fn prove_artifacts_on_disk(ctx: &CutCtx) -> Result<Manifest> {
    prove_bundle_on_disk(ctx)?;
    prove_publication_on_disk(ctx)
}

fn prove_bundle_on_disk(ctx: &CutCtx) -> Result<()> {
    let app = ctx.app_path();

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

fn prove_publication_on_disk(ctx: &CutCtx) -> Result<Manifest> {
    if let Some(handoff) = &ctx.linux {
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

    let provenance = fs::read(ctx.provenance_path())
        .map_err(|error| Error::new(format!("read release provenance: {error}")))?;
    validate_claim_provenance(&provenance, &ctx.version, ctx.build, &ctx.commit)?;

    if ctx.signature_required {
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

fn step_selfcheck(ctx: &mut CutCtx) -> Result<()> {
    let manifest = prove_artifacts_on_disk(ctx)?;
    let app = ctx.app_path();

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
    let team = manifest.team_id.clone().unwrap_or_default();
    let apple_note = selfcheck_signing(&team, &app, &ctx.dmg_path(), &sign::RealAppleTools)?;
    step(
        "selfcheck",
        &format!(
            "binary == plist == manifest == {} · codesign --verify --deep --strict ok{apple_note}",
            ctx.build
        ),
    );

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

/// Proof that the release channel is reachable, public and writable by this operator's credential.
pub fn preflight_channel_target(slug: &str, claimed: bool) -> Result<()> {
    let repository = crate::github::get(&format!("repos/{slug}")).map_err(|error| {
        Error::new(format!(
            "cannot read the release channel {slug}: {error}. A 404 here means the \
             repository does not exist or this credential cannot see it; create it (public) \
             and grant the release account write access."
        ))
    })?;
    // `.permissions` is GitHub's answer for the identity that asked; an anonymous read
    // carries none, so it never reads as push.
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
        let token_path = channel_token_path().map_or_else(
            || "~/.secrets/gh_access_token_alabsystems".to_string(),
            |path| path.display().to_string(),
        );
        // With no token loaded the read went anonymously: the token was never asked.
        return Err(Error::new(if crate::github::has_token() {
            format!(
                "the release token ({token_path}) cannot write {slug}: grant it write access{then}"
            )
        } else {
            format!(
                "no release token in {token_path}, and an anonymous read cannot write {slug}: \
                 put the release-org token there{then}"
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
                 refusing (channel::publish_on_channel binds right after the floors)",
            )
        })
    }
}

impl channel::ChannelRelease for LiveChannelRelease<'_> {
    fn head_made(&mut self) -> Result<bool> {
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
        let (release_id, adopted) = self.bound()?;
        prove_channel_assets(self.ctx, self.slug, release_id, !adopted)
    }

    fn ratchet_head(&mut self) -> Result<()> {
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
            &mut anonymous_get_optional,
        )
    }

    fn ratchet(&mut self) -> Result<()> {
        self.ratchet_head()?;
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
        crate::github::send(
            aterm_update_core::github_rest::Method::Patch,
            &format!("repos/{slug}/releases/{release_id}"),
            Some(&channel::head_patch_body()),
            || {
                let current = release_object_by_id(slug, release_id)?;
                validate_channel_release_capability(
                    current.as_ref(),
                    release_id,
                    &ctx.tag,
                    expect_draft,
                )?;
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
        let (release_id, _) = self.bound()?;
        let slug = self.slug;
        let ctx = &*self.ctx;
        let after = release_object_by_id(slug, release_id)?;
        validate_channel_release_capability(after.as_ref(), release_id, &ctx.tag, false)?;
        prove_client_elects_this_cut(ctx, slug, release_id)?;
        prove_evergreen_pointer_serves_this_cut(ctx, slug)?;
        prove_channel_assets_download_anonymously(ctx, slug)
    }
}

const ANON_PROBE_ATTEMPTS: u32 = 10;

const ANON_PROBE_DELAY: Duration = Duration::from_secs(6);

/// Attempts for the evergreen pointer to name this cut once its head PATCH is sent ([`prove_pointer_serves`]).
pub const POINTER_PROBE_ATTEMPTS: u32 = 30;

/// Gap between evergreen-pointer attempts.
pub const POINTER_PROBE_DELAY: Duration = Duration::from_secs(10);

/// The HTTP status of a refused anonymous probe (403 or 429).
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

fn anon_probe(args: &[&str]) -> Result<std::process::Output> {
    let mut last = None;
    for attempt in 1..=ANON_PROBE_ATTEMPTS {
        let out = Command::new("curl")
            .args(args)
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

/// The readability proof.
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

/// What the evergreen pointer (`https://github.com/<slug>/releases/latest/download/<asset>`) answers a credential-less client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PointerProbe {
    Tag { tag: String, location: String },
    NoRelease,
    Refused { code: u16, reason: String },
    NotAnApp { tag: String },
    Other(String),
}

/// One anonymous.
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
        if matches!(last, PointerProbe::Refused { .. }) {
            break;
        }
        if attempt < POINTER_PROBE_ATTEMPTS {
            sleep(POINTER_PROBE_DELAY);
        }
    }
    let Some(location) = location else {
        if let PointerProbe::Refused { code, reason } = &last {
            // Distinct branch.
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

fn anonymous_get(location: &str) -> Result<Vec<u8>> {
    anonymous_get_optional(location)?.ok_or_else(|| {
        Error::new(format!(
            "an unauthenticated GET of {location} answered 404 for the whole retry budget"
        ))
    })
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

fn create_channel_draft(ctx: &mut CutCtx, slug: &str) -> Result<ReleaseObjectIdentity> {
    let notes = release_body_for_post("publish", &ctx.notes_path())?;
    let posted_body_len = notes.len();
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
    let post = OneShotPost::prepare_binary(&endpoint, file)?;
    let release = release_object_by_id(slug, release_id)?;
    validate_channel_release_capability(release.as_ref(), release_id, &ctx.tag, expect_draft)?;
    ensure_ctx_release_lease(ctx)?;
    let permit = ctx.persist_upload_intent(name)?;
    let out = post.issue(permit)?;
    let visible = || {
        release_asset_identity_for_release_id_optional(slug, release_id, name)
            .map(|found| found.is_some())
    };
    match after_upload_post(|| ctx.settle_upload_intent(name, &out), visible)? {
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

/// The upload path of asset `name` on release `release_id` of `slug`, on the uploads
/// origin: every part canonical, so nothing in it can reach another endpoint.
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
mod release_body_post_tests {

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
        assert_eq!(
            fs::read_to_string(&path).expect("re-read fixture").len(),
            oversized.len()
        );
    }
}

#[cfg(test)]
mod transport_body_tests {

    use super::*;

    /// A one-shot POST prepared against a loopback GitHub with a token (nothing is sent).
    fn prepared(body: impl FnOnce() -> Result<OneShotPost>) -> Vec<String> {
        let _github = crate::github::Fixture {
            origins: aterm_update_core::github_rest::Origins::loopback(1, 2, 3, 4),
            token: Some("ghs_fixtureOneShot0123".to_string()),
            spawn: None,
        }
        .enter();
        body().expect("prepared").argv()
    }

    #[test]
    fn an_asset_upload_streams_off_disk_and_never_buffers() {
        let dir = PrivateTempDir::create(std::env::temp_dir().join(format!(
            "aterm-release-oneshot-{}-{}",
            std::process::id(),
            RELEASE_ASSET_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        )))
        .expect("scratch");
        let dmg = dir.path().join("aterm-0.33.0.dmg");
        fs::write(&dmg, b"dmg").expect("the asset");
        let args = prepared(|| {
            OneShotPost::prepare_binary("repos/o/r/releases/1/assets?name=aterm-0.33.0.dmg", &dmg)
        });
        assert!(
            args.iter().any(|a| a == "--upload-file"),
            "the asset leg must stream: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--data-binary"),
            "a gigabyte payload must never be buffered: {args:?}"
        );
        let request = args.iter().position(|a| a == "-X").unwrap();
        assert_eq!(args[request + 1], "POST");
        // No `@` prefix: that spelling is --data-binary's, and curl would look for a file named "@/…".
        let flag = args.iter().position(|a| a == "--upload-file").unwrap();
        assert_eq!(args[flag + 1], dmg.to_str().unwrap());
        assert_eq!(
            args.last().map(String::as_str),
            Some("http://127.0.0.1:2/repos/o/r/releases/1/assets?name=aterm-0.33.0.dmg"),
            "the uploads origin"
        );
        // A JSON body still buffers from its private temp file, and either way the token
        // is read from stdin, never the argv.
        let args = prepared(|| OneShotPost::prepare_json("repos/o/r/releases", b"{}"));
        let flag = args.iter().position(|a| a == "--data-binary").unwrap();
        assert!(args[flag + 1].starts_with('@'), "{args:?}");
        assert!(!args.iter().any(|a| a == "--upload-file"));
        assert!(args.iter().any(|a| a == "--fail-with-body"), "{args:?}");
        assert!(args.windows(2).any(|w| w == ["-H", "@-"]), "{args:?}");
        assert!(
            !args.iter().any(|a| a.contains("ghs_fixtureOneShot0123")),
            "{args:?}"
        );
        assert_eq!(args.first().map(String::as_str), Some("/usr/bin/curl"));
    }

    fn out(status: i32, stderr: &str) -> RunOut {
        RunOut {
            status,
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

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

    use super::*;

    #[test]
    fn a_recovery_verifies_under_the_release_key_and_signs_under_its_own() {
        let mut ctx = ctx(Some("m3"));
        ctx.signature_pubkey = Some("Kb-this-machine".to_string());
        assert_eq!(
            ctx.verification_pubkey(),
            Some("Kb-this-machine"),
            "an ordinary cut verifies under the key it signs with"
        );

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
            assert!(set.contains(&"aterm-appcast.toml".to_string()), "{set:?}");
        }

        let plain = ctx(None);
        assert!(!plain.attaches_roster());
        let set = names(&plain.channel_asset_paths());
        assert!(
            !set.iter().any(|n| n.starts_with("aterm-machines")),
            "an unattributed cut must publish no roster: {set:?}"
        );
    }

    #[test]
    fn the_pin_expectation_is_the_committed_master_whoever_signs() {
        let master = aterm_update_core::pins::PAPER_MASTER_PUBKEYS
            .first()
            .copied()
            .expect("precondition: this tree pins a paper master");
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
        assert_eq!(cut_roster_seq(&ctx(None)).unwrap(), None);
        let mut lost = ctx(Some("m3"));
        lost.dist = dir.join("gone");
        assert!(cut_roster_seq(&lost).is_err());

        let _ = fs::remove_dir_all(&dir);
    }

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

    #[test]
    fn recovery_refuses_to_overwrite_a_newer_local_roster() {
        let dir =
            std::env::temp_dir().join(format!("aterm-roster-downgrade-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        assert!(refuse_roster_downgrade(&dir, 1).is_ok());

        let bytes = aterm_codec::base64::decode_strict(ROSTER_SEQ2.concat().as_bytes()).unwrap();
        let sig = aterm_codec::base64::decode_strict(ROSTER_SEQ2_SIG.as_bytes()).unwrap();
        std::fs::write(dir.join(roster::ROSTER_ASSET), &bytes).unwrap();
        std::fs::write(dir.join(roster::ROSTER_SIG_ASSET), &sig).unwrap();

        let err = refuse_roster_downgrade(&dir, 1).unwrap_err();
        assert!(err.0.contains("NEWER"), "{}", err.0);
        assert!(err.0.contains(roster::ROSTER_ASSET), "{}", err.0);

        assert!(refuse_roster_downgrade(&dir, 2).is_ok());
        assert!(refuse_roster_downgrade(&dir, 3).is_ok());

        std::fs::write(dir.join(roster::ROSTER_SIG_ASSET), [0u8; 64]).unwrap();
        assert!(refuse_roster_downgrade(&dir, 1).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

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

    const LEAN_0_67_0: u64 = 31_018_076; // the shipped lean image
    const SEEDED_0_63_0: u64 = 1_067_044_990; // a seeded image

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
        assert!(validate_lean_dmg_size("0.64.0", 0).is_ok());
        assert!(validate_release_asset_download_size(0).is_err());
    }

    #[test]
    fn an_oversized_resumed_dmg_is_refused_from_the_actual_file() {
        let (_dir, path) = sparse_dmg("resume", LEAN_DMG_CEILING_BYTES + 1);
        validate_lean_dmg_on_disk(&path, "self-check", Some("abandon"))
            .expect_err("resume must re-check the current dist DMG");
    }

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
                size: 1,
                sha256: "00".repeat(32),
            })
        })
        .expect_err("recovery must reject the retained seeded image");
    }

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
        // Negative control: the predicate flags an API-host URL (`<api origin>/repos/<slug>/releases/tags/<tag>`).
        let retired = format!(
            "{}/repos/{SLUG}/releases/tags/{TAG}",
            aterm_update_core::github_rest::Origins::github().api
        );
        assert!(aterm_update_core::cdn::is_api_host(&retired));
    }

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

    use super::*;

    const VERSION: &str = "0.5.0";
    const BUILD: u64 = 500;

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
        assert!(ctx.stable_dmg_path().is_file() && ctx.dmg_sha256_path().is_file());

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
