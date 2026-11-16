// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Mac half of the Windows pull lane (docs/RELEASING.md § Windows release handoff, *The
//! pull lane*): how a release whose cut declares no Windows pair asks a Windows builder for one
//! and admits what it hands back, with no person carrying anything. The wire is aterm-winship's
//! one definition (`aterm_winship::pull`, `aterm_winship::attest`), which `release-serve` on the
//! Windows host writes and reads, so both ends agree by compilation.
//!
//! * At reservation the release writes the request ([`write_request`]): origin's
//!   `refs/aterm-release/windows/request` becomes a commit whose one parent is the release commit
//!   C, naming the version, build, C, C's tree and when it was asked ([`Pull`] journals that). A
//!   builder builds only a C whose parent is on origin/main; a line carrying `--fix` hops is too,
//!   since the release records its head on main before it reserves.
//! * Before step 7 first signs, while the cut's Windows declaration is still open, the release
//!   looks ([`look`]): the handoff ref of (version, build, C), fetched, put back together in a
//!   private directory, held to its parts table, admitted as a hand-carried pair is
//!   (`aterm_winship::handoff::admit` under C's pins) and its receipt held to the attestation of
//!   a builder C itself pins (`apps/aterm-win/release-builders.toml`), unexpired on the UTC day
//!   it is admitted, which is journaled ([`Pull::admitted_on`]) and is the day every later check
//!   of that attestation is judged on; else the builder's heartbeat. [`decide`] says what that
//!   means: admit the pair, wait (a builder C pins is serving and has not passed this request
//!   over, or origin could not be read, and the wait from the request has not run out: the run
//!   ends COULD NOT RUN, exit 3, and the pump runs it again on the same reservation), or go on
//!   without a Windows zip.
//!
//! The lane fails open: whatever goes wrong in it (a part missing, a digest that disagrees, a
//! signature that does not verify, a builder C does not pin or pins no longer, origin unreadable
//! past the wait) is journaled and said, and the cut goes on Mac-only; it never refuses a
//! release. A bad signature is said as such, loudly. What origin's refs say reaches the output
//! and the journal with its control characters escaped: anyone who writes those refs writes it.

use std::path::{Path, PathBuf};

use aterm_winship::attest::{self, Builders, Fault};
use aterm_winship::handoff::{self, Cut};
use aterm_winship::pull::{self, Alive, Request};
use serde::{Deserialize, Serialize};

use super::windows::Sources;
use crate::ledger::{GitRunner, git_ok};

/// `publish/config.sh`'s key that turns the pull lane on (`"1"`).
pub const PULL_KEY: &str = "WINDOWS_PULL_DEFAULT";
/// `publish/config.sh`'s key for how many seconds after the request step 7 waits at most.
pub const WAIT_KEY: &str = "WINDOWS_PULL_WAIT_DEFAULT";
/// `publish/config.sh`'s key for how old, in seconds, a heartbeat may be and still say a builder
/// is serving.
pub const ALIVE_KEY: &str = "WINDOWS_PULL_ALIVE_DEFAULT";
/// [`WAIT_KEY`]'s value where the file names none.
pub const WAIT_SECS: u64 = 5400;
/// [`ALIVE_KEY`]'s value where the file names none.
pub const ALIVE_SECS: u64 = 1800;
/// The folder in the release directory where a handoff is put back together, one folder a key
/// (`aterm_winship::pull::handoff_key`).
pub const PULL_DIR: &str = "windows-pull";

/// The pull lane's configuration, as the release commit's `publish/config.sh` says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// [`PULL_KEY`] is `"1"`.
    pub on: bool,
    /// [`WAIT_KEY`], seconds.
    pub wait: u64,
    /// [`ALIVE_KEY`], seconds.
    pub alive: u64,
}

impl Config {
    /// No pull lane.
    pub const OFF: Self = Self {
        on: false,
        wait: WAIT_SECS,
        alive: ALIVE_SECS,
    };

    /// The configuration `config` (a `publish/config.sh`) sets: its flat `KEY="value"` lines,
    /// read as the pump reads its own (`aterm_release::pump::config_value`). A number that does
    /// not read is the default.
    #[must_use]
    pub fn read(config: &str) -> Self {
        let value = |key: &str| aterm_release::pump::config_value(config, key);
        let secs = |key: &str, default: u64| {
            value(key)
                .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        };
        Self {
            on: value(PULL_KEY) == Some("1"),
            wait: secs(WAIT_KEY, WAIT_SECS),
            alive: secs(ALIVE_KEY, ALIVE_SECS),
        }
    }
}

/// What the release journal keeps of the pull for one reservation: the request, and once step 7
/// decided, what it decided. A decision stands for the reservation: the cut's Windows
/// declaration freezes with the first private draft, so a handoff that arrives after a Mac-only
/// decision is not added.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pull {
    /// The reservation's build.
    pub build: u64,
    /// The reservation's release commit C.
    pub commit: String,
    /// Epoch seconds the request was made: the wait runs from here.
    pub requested_at: u64,
    /// The request commit origin's request ref was set to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
    /// Why the request could not be written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unrequested: Option<String>,
    /// The builder whose handoff step 7 admitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admitted: Option<String>,
    /// The UTC day (`YYYY-MM-DD`) step 7 admitted it: its attestation's expiry is judged on
    /// this day and never on a later one, so a resume after the builder's `not_after` signs
    /// what was admitted before it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admitted_on: Option<String>,
    /// Where the admitted pair was put back together: the cut's `--windows-artifacts`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<PathBuf>,
    /// Why step 7 goes on without a Windows zip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

impl Pull {
    /// The record of a request for build `build` at `commit`, made at `requested_at`.
    #[must_use]
    pub fn new(build: u64, commit: &str, requested_at: u64) -> Self {
        Self {
            build,
            commit: commit.to_string(),
            requested_at,
            ..Self::default()
        }
    }

    /// It is the pull of build `build` at `commit`.
    #[must_use]
    pub fn of(&self, build: u64, commit: &str) -> bool {
        self.build == build && self.commit == commit
    }

    /// Step 7 decided it.
    #[must_use]
    pub fn decided(&self) -> bool {
        self.admitted.is_some() || self.skipped.is_some()
    }
}

/// Where the pull lane reads and writes origin: the release's own typed transport in a real run
/// ([`RemoteOrigin`]), a local bare repository in a test.
pub trait Origin {
    /// The object origin's `reference` names; `None` without one.
    ///
    /// # Errors
    /// Origin could not be read.
    fn get(&self, reference: &str) -> Result<Option<String>, String>;
    /// Brings the object `oid` origin advertises, and all it reaches, into the local repository.
    ///
    /// # Errors
    /// The fetch failed.
    fn fetch(&self, oid: &str) -> Result<(), String>;
    /// Points `reference` at the local commit `after`, where origin's is `before` (`None`: has
    /// none). `Ok(false)`: origin's was not `before`, and nothing changed.
    ///
    /// # Errors
    /// The write failed otherwise.
    fn write(&self, reference: &str, before: Option<&str>, after: &str) -> Result<bool, String>;
    /// Deletes `reference`, where origin's is `before`. `Ok(false)`: it was not.
    ///
    /// # Errors
    /// The delete failed otherwise.
    fn delete(&self, reference: &str, before: &str) -> Result<bool, String>;
}

/// [`Origin`] over the release's typed GitHub transport (`crate::github_remote`): agh's exact
/// reads, imports and expected-old ref writes, as the fleet line claim makes them.
pub(crate) struct RemoteOrigin<'a>(pub(crate) &'a crate::github_remote::Remote);

impl Origin for RemoteOrigin<'_> {
    fn get(&self, reference: &str) -> Result<Option<String>, String> {
        self.0.get(reference)
    }

    fn fetch(&self, oid: &str) -> Result<(), String> {
        self.0.fetch_exact(oid)
    }

    fn write(&self, reference: &str, before: Option<&str>, after: &str) -> Result<bool, String> {
        match self.0.write_ref(reference, before, after)? {
            crate::github_remote::RefWrite::Written => Ok(true),
            crate::github_remote::RefWrite::Conflict => Ok(false),
            crate::github_remote::RefWrite::Transient(why) => Err(why),
        }
    }

    fn delete(&self, reference: &str, before: &str) -> Result<bool, String> {
        match self.0.delete_ref(reference, before)? {
            crate::github_remote::RefWrite::Written => Ok(true),
            crate::github_remote::RefWrite::Conflict => Ok(false),
            crate::github_remote::RefWrite::Transient(why) => Err(why),
        }
    }
}

fn git_text(git: &dyn GitRunner, args: &[&str]) -> Result<String, String> {
    git_ok(git, args)
        .map(|out| out.stdout_utf8().trim().to_string())
        .map_err(|e| e.to_string())
}

fn git_with_input(git: &dyn GitRunner, args: &[&str], input: &[u8]) -> Result<String, String> {
    let out = git.git_input(args, input).map_err(|e| e.to_string())?;
    if !out.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            out.stderr_utf8().trim()
        ));
    }
    Ok(out.stdout_utf8().trim().to_string())
}

/// The request of release `version` build `build` at the release commit `commit` (in `git`'s
/// repository), made at `now` (epoch seconds): C's tree read from the repository.
///
/// # Errors
/// C's tree does not read.
pub fn request(
    git: &dyn GitRunner,
    version: &str,
    build: u64,
    commit: &str,
    now: u64,
) -> Result<Request, String> {
    let tree = git_text(
        git,
        &["rev-parse", "--verify", &format!("{commit}^{{tree}}")],
    )?;
    let request = Request {
        version: version.to_string(),
        build,
        commit: commit.to_string(),
        tree,
        reserved_at: aterm_winship::pointer::rfc3339(now),
    };
    request.check()?;
    Ok(request)
}

/// The request commit of `request`, made from objects alone in `git`'s repository under its
/// configured identity, as the fleet line claim makes its own: a tree of
/// `aterm_winship::pull::REQUEST_FILE` alone and the release commit as its one parent.
///
/// # Errors
/// A git failure.
pub fn request_commit(git: &dyn GitRunner, request: &Request) -> Result<String, String> {
    let text = request.to_toml()?;
    let blob = git_with_input(git, &["hash-object", "-w", "--stdin"], text.as_bytes())?;
    let entry = format!("100644 blob {blob}\t{}\0", pull::REQUEST_FILE);
    let tree = git_with_input(git, &["mktree", "-z"], entry.as_bytes())?;
    let message = format!(
        "windows request: v{} build {} at {}\n",
        request.version, request.build, request.commit
    );
    git_with_input(
        git,
        &["commit-tree", &tree, "-p", &request.commit],
        message.as_bytes(),
    )
}

/// Sets origin's request ref to `commit`. The ref only moves to a descendant through a plain
/// write, and a request of another reservation is no descendant of the last one: one that is
/// there is deleted (as it was read) first. Nothing reads the ref's absence in between as more
/// than no request.
///
/// # Errors
/// Origin could not be read or written, or another writer moved the ref meanwhile.
pub fn replace_request(origin: &dyn Origin, commit: &str) -> Result<(), String> {
    let refname = pull::REQUEST_REF;
    match origin.get(refname)? {
        Some(old) if old == commit => return Ok(()),
        Some(old) if !origin.delete(refname, &old)? => {
            return Err(format!("{refname} moved while this release replaced it"));
        }
        Some(_) | None => {}
    }
    if origin.write(refname, None, commit)? {
        Ok(())
    } else {
        Err(format!(
            "{refname} was written by another writer while this release wrote it"
        ))
    }
}

/// Asks origin for build `build`'s Windows zip: the request of release `version` build `build`
/// at `commit`, made at `now` ([`request`], [`request_commit`]), set as origin's request ref
/// ([`replace_request`]). The request commit.
///
/// # Errors
/// What [`request`], [`request_commit`] or [`replace_request`] refuses.
pub fn write_request(
    origin: &dyn Origin,
    git: &dyn GitRunner,
    version: &str,
    build: u64,
    commit: &str,
    now: u64,
) -> Result<String, String> {
    let asked = request(git, version, build, commit, now)?;
    let oid = request_commit(git, &asked)?;
    replace_request(origin, &oid)?;
    Ok(oid)
}

/// The reservation a pull is of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    /// The release version.
    pub version: String,
    /// The reserved build.
    pub build: u64,
    /// The release commit C.
    pub commit: String,
}

impl Ask {
    /// Its handoff ref (`aterm_winship::pull::handoff_ref`).
    #[must_use]
    pub fn handoff_ref(&self) -> String {
        pull::handoff_ref(&self.version, self.build, &self.commit)
    }

    /// Its folder under `release` (the release directory): [`PULL_DIR`] and the handoff key.
    #[must_use]
    pub fn directory(&self, release: &Path) -> PathBuf {
        release
            .join(PULL_DIR)
            .join(pull::handoff_key(&self.version, self.build, &self.commit))
    }
}

/// A handoff admitted: the pair and its attestation in `directory`, and who attested it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pulled {
    /// The private directory holding the zip, its receipt and the attestation, and nothing else:
    /// what the cut is handed as its Windows handoff.
    pub directory: PathBuf,
    /// The zip's name, as its receipt names it.
    pub asset: String,
    /// The zip's SHA-256, as its receipt (and the parts table) names it.
    pub sha256: String,
    /// The builder whose attestation verified, as C pins it.
    pub builder: String,
    /// Its pinned public key.
    pub public_key: String,
}

/// A handoff refused: why, whether it was a signature that does not verify, and whether it was
/// only not had (origin could not be fetched, this Mac could not make the folder or read C),
/// which a later run may get past.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// Why, for a person.
    pub why: String,
    /// The attestation is not the pinned key's over this receipt (`Fault::BadSignature`).
    pub bad_signature: bool,
    /// The handoff was not had, rather than refused for what it holds.
    pub transient: bool,
}

impl Refused {
    fn plain(why: impl Into<String>) -> Self {
        Self {
            why: why.into(),
            bad_signature: false,
            transient: false,
        }
    }

    fn transient(why: impl Into<String>) -> Self {
        Self {
            transient: true,
            ..Self::plain(why)
        }
    }
}

/// The heartbeat could not be had: why, and whether origin could not be read (`transient`)
/// rather than what it holds is no heartbeat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unread {
    /// Why, for a person.
    pub why: String,
    /// Origin could not be read or fetched.
    pub transient: bool,
}

impl Unread {
    fn transient(why: String) -> Self {
        Self {
            why,
            transient: true,
        }
    }

    fn content(why: String) -> Self {
        Self {
            why,
            transient: false,
        }
    }
}

/// The builder's heartbeat, as read from origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heartbeat {
    /// Its record.
    pub alive: Alive,
    /// Its commit's committer time, epoch seconds.
    pub committed: u64,
    /// Whether the release commit pins its builder, unexpired on the day the pull is judged:
    /// a builder it does not pin hands off nothing this release admits. `Err`: why not.
    pub pinned: Result<(), String>,
}

impl Heartbeat {
    /// Written within `bound` seconds of `now`, by its record's time and by its commit's, either
    /// way.
    #[must_use]
    pub fn fresh(&self, now: u64, bound: u64) -> bool {
        self.alive.fresh(now, bound) && now.abs_diff(self.committed) <= bound
    }

    /// Seconds from its record's time to `now` (0 for one ahead of `now`).
    #[must_use]
    pub fn age(&self, now: u64) -> u64 {
        self.alive
            .at_secs()
            .map_or(u64::MAX, |at| now.saturating_sub(at))
    }
}

/// What origin holds for a pull.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// The handoff ref: the pair admitted, or why not.
    Handoff(Result<Pulled, Refused>),
    /// No handoff ref; the heartbeat (`None`: origin holds none), or why it could not be read.
    Absent(Result<Option<Heartbeat>, Unread>),
}

/// The standard output of `git args`, its whole bytes: a handoff's blobs are up to 45 MiB.
fn git_bytes(git: &dyn GitRunner, args: &[&str]) -> Result<Vec<u8>, String> {
    git_ok(git, args)
        .map(|out| out.stdout)
        .map_err(|e| e.to_string())
}

/// A fresh private (owner-only) directory at `path`, whatever was there gone.
fn fresh_private_dir(path: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", path.display())),
    }
    private_dir(path)
}

/// `path` and every missing folder above it made owner-only.
fn private_dir(path: &Path) -> Result<(), String> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(path)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Whether the release commit `commit` pins `builder`, unexpired on `day` (`YYYY-MM-DD`).
fn pinned_builder(
    git: &dyn GitRunner,
    commit: &str,
    builder: &str,
    day: &str,
) -> Result<(), String> {
    let text = super::windows::builders_at(git, commit)?.ok_or_else(|| {
        format!(
            "the release commit holds no {}: it pins no Windows builder",
            pull::BUILDERS_FILE
        )
    })?;
    Builders::parse(&text)
        .and_then(|builders| builders.current(builder, day).map(drop))
        .map_err(|e| e.to_string())
}

/// The heartbeat origin holds, fetched and read, and whether the release commit of `ask` pins
/// its builder unexpired on `day` (`YYYY-MM-DD`); `None` without one.
///
/// # Errors
/// Origin could not be read or fetched (`transient`), or what it holds is no heartbeat.
pub fn heartbeat(
    origin: &dyn Origin,
    git: &dyn GitRunner,
    ask: &Ask,
    day: &str,
) -> Result<Option<Heartbeat>, Unread> {
    let Some(oid) = origin.get(pull::ALIVE_REF).map_err(Unread::transient)? else {
        return Ok(None);
    };
    origin.fetch(&oid).map_err(Unread::transient)?;
    let spec = format!("{oid}:{}", pull::ALIVE_FILE);
    let size: u64 = git_text(git, &["cat-file", "-s", &spec])
        .map_err(Unread::content)?
        .parse()
        .map_err(|_| Unread::content(format!("git cat-file -s {spec} wrote no size")))?;
    if size > pull::RECORD_MAX_BYTES {
        return Err(Unread::content(format!(
            "{spec} is {size} bytes, oversized"
        )));
    }
    let text =
        String::from_utf8(git_bytes(git, &["cat-file", "blob", &spec]).map_err(Unread::content)?)
            .map_err(|_| Unread::content(format!("{spec} is not UTF-8")))?;
    let alive = Alive::parse(&text).map_err(Unread::content)?;
    let committed = git_text(git, &["show", "-s", "--format=%ct", &oid])
        .map_err(Unread::content)?
        .parse()
        .map_err(|_| {
            Unread::content(format!("the heartbeat commit {oid} has no committer time"))
        })?;
    let pinned = pinned_builder(git, &ask.commit, &alive.builder, day);
    Ok(Some(Heartbeat {
        alive,
        committed,
        pinned,
    }))
}

/// The handoff commit `oid` (advertised by origin) fetched, written out and put back together in
/// `directory` (made afresh: `parts` for its files, `carry` for the pair), admitted as the cut
/// `ask` admits a hand-carried pair under C's pins, and its receipt held to the attestation of a
/// builder C pins, not expired on `day` (`YYYY-MM-DD`, UTC).
///
/// # Errors
/// What refuses it, first: the fetch, the commit's shape, the parts against their table, C's
/// pins (its builders file absent included), admission, the attestation. One that was not had
/// (the fetch, this Mac's folder, C unreadable here) is `transient`.
pub fn admit(
    origin: &dyn Origin,
    git: &dyn GitRunner,
    ask: &Ask,
    oid: &str,
    directory: &Path,
    day: &str,
) -> Result<Pulled, Refused> {
    origin.fetch(oid).map_err(Refused::transient)?;
    let parts = directory.join("parts");
    let carry = directory.join("carry");
    fresh_private_dir(directory).map_err(Refused::transient)?;
    private_dir(&parts).map_err(Refused::transient)?;
    private_dir(&carry).map_err(Refused::transient)?;
    pull::extract(&mut |args| git_bytes(git, args), oid, &parts).map_err(Refused::plain)?;
    let assembled = pull::assemble(&parts, &carry).map_err(Refused::plain);
    // The pair is in `carry` now (or nothing is): the parts are spent either way.
    let _ = std::fs::remove_dir_all(&parts);
    let table = assembled?;
    let sources = Sources::at(git, &ask.commit).map_err(Refused::transient)?;
    let pinned = sources.builders.as_deref().ok_or_else(|| {
        Refused::plain(format!(
            "the release commit {} holds no {}: it pins no Windows builder",
            ask.commit,
            pull::BUILDERS_FILE
        ))
    })?;
    let builders = Builders::parse(pinned).map_err(|e| Refused::plain(e.to_string()))?;
    let cut = Cut {
        version: &ask.version,
        build: ask.build,
        commit: &ask.commit,
        source_tree: &sources.tree,
    };
    let receipt = handoff::admit(&carry, &cut, &sources.pins()).map_err(Refused::plain)?;
    if receipt.sha256 != table.sha256 || receipt.size != table.size {
        return Err(Refused::plain(format!(
            "the handoff's parts table names {} bytes of SHA-256 {}, and its receipt {} bytes \
             of {}",
            table.size, table.sha256, receipt.size, receipt.sha256
        )));
    }
    let attested =
        attest::verify_beside(&carry, &receipt.asset, &builders, day).map_err(|refused| {
            Refused {
                why: refused.to_string(),
                bad_signature: refused.fault == Fault::BadSignature,
                transient: false,
            }
        })?;
    Ok(Pulled {
        directory: carry,
        asset: receipt.asset,
        sha256: receipt.sha256,
        builder: attested.builder,
        public_key: attested.public_key,
    })
}

/// What origin holds for `ask`, judged on `day` (`YYYY-MM-DD`, UTC: the day a pair is admitted,
/// or was): its handoff admitted ([`admit`], put together under `directory`) or refused, else
/// the heartbeat.
///
/// # Errors
/// The handoff ref could not be read.
pub fn look(
    origin: &dyn Origin,
    git: &dyn GitRunner,
    ask: &Ask,
    directory: &Path,
    day: &str,
) -> Result<Found, String> {
    Ok(match origin.get(&ask.handoff_ref())? {
        Some(oid) => Found::Handoff(admit(origin, git, ask, &oid, directory, day)),
        None => Found::Absent(heartbeat(origin, git, ask, day)),
    })
}

/// What the folder `directory` of a pair admitted for release `version` lacks of it: the zip,
/// its receipt and its attestation, by name (or why the folder cannot be read).
#[must_use]
pub fn lacks(directory: &Path, version: &str) -> Vec<String> {
    let mut lacking = handoff::missing(directory, version).unwrap_or_else(|e| vec![e]);
    let attestation = pull::attestation_name(&handoff::TARGET.asset_name(version));
    if std::fs::symlink_metadata(directory.join(&attestation)).is_err() {
        lacking.push(attestation);
    }
    lacking
}

/// `text` with every control character escaped (`\u{1b}` for ESC): what origin's refs say
/// reaches the release's output and its journal, and anyone who writes those refs writes it.
fn plain(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// What step 7 does about the pull.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Declare the admitted pair.
    Admit(Pulled),
    /// End the run COULD NOT RUN: a builder is serving and has until `until` (epoch seconds).
    Wait {
        /// When the wait runs out.
        until: u64,
        /// Why it waits.
        why: String,
    },
    /// Go on without a Windows zip.
    Skip {
        /// Why.
        why: String,
        /// A signature that did not verify: said loudly.
        loud: bool,
    },
}

/// The decision of the pull `pull` at `now` under `config`, from what origin was found to hold
/// (`found`): a handoff admitted is declared; one refused for what it holds, or none when no
/// builder's heartbeat is fresh, its builder is not one the release commit pins, it passed this
/// request over, the request was never written or the wait from the request has run out, is
/// gone without; else (a builder serving, or origin not had: unreadable, a fetch that failed)
/// the run waits, until the wait from the request runs out. Every reason has its control
/// characters escaped ([`plain`]).
#[must_use]
pub fn decide(found: Result<Found, String>, now: u64, pull: &Pull, config: &Config) -> Decision {
    match decided(found, now, pull, config) {
        Decision::Wait { until, why } => Decision::Wait {
            until,
            why: plain(&why),
        },
        Decision::Skip { why, loud } => Decision::Skip {
            why: plain(&why),
            loud,
        },
        admit @ Decision::Admit(_) => admit,
    }
}

fn decided(found: Result<Found, String>, now: u64, pull: &Pull, config: &Config) -> Decision {
    let skip = |why: String| Decision::Skip { why, loud: false };
    let deadline = pull.requested_at.saturating_add(config.wait);
    // What origin did not give is no answer: a run that could not read it waits, as for a
    // builder at work, and decides nothing it would keep until the wait runs out.
    let not_had = |why: String| {
        if now < deadline {
            Decision::Wait {
                until: deadline,
                why: format!("{why}; a later run reads origin again"),
            }
        } else {
            skip(format!(
                "{why}, and the wait {WAIT_KEY} allows from the request ({} s) has run out",
                config.wait
            ))
        }
    };
    let found = match found {
        Ok(found) => found,
        Err(why) if pull.unrequested.is_none() => {
            return not_had(format!("origin could not be read for the handoff: {why}"));
        }
        Err(why) => return skip(format!("origin could not be read for the handoff: {why}")),
    };
    let alive = match found {
        Found::Handoff(Ok(pulled)) => return Decision::Admit(pulled),
        Found::Handoff(Err(refused)) if refused.transient => {
            return not_had(format!(
                "the handoff on origin could not be had: {}",
                refused.why
            ));
        }
        Found::Handoff(Err(refused)) => {
            return Decision::Skip {
                why: format!("the handoff on origin is refused: {}", refused.why),
                loud: refused.bad_signature,
            };
        }
        Found::Absent(alive) => alive,
    };
    if let Some(why) = &pull.unrequested {
        return skip(format!(
            "no handoff is on origin, and the request was never written ({why})"
        ));
    }
    // What the heartbeat's builder made of this very request, when it passed it over.
    let passed_over = match &alive {
        Ok(Some(beat)) => pull
            .request
            .as_deref()
            .and_then(|request| beat.alive.passed_over(request)),
        _ => None,
    };
    match alive {
        Err(unread) if unread.transient => not_had(format!(
            "no handoff is on origin, and the builder heartbeat could not be read: {}",
            unread.why
        )),
        Err(unread) => skip(format!(
            "no handoff is on origin, and the builder heartbeat could not be read: {}",
            unread.why
        )),
        Ok(None) => {
            skip("no handoff is on origin, and no Windows builder is serving (no heartbeat)".into())
        }
        Ok(Some(beat)) if !beat.fresh(now, config.alive) => skip(format!(
            "no handoff is on origin, and builder {}'s last heartbeat ({}) is not within {} s \
             of now: no builder is serving",
            beat.alive.builder, beat.alive.at, config.alive
        )),
        Ok(Some(Heartbeat {
            alive,
            pinned: Err(why),
            ..
        })) => skip(format!(
            "no handoff is on origin, and builder {} is serving, but the release commit does \
             not admit its attestation: {why}",
            alive.builder
        )),
        Ok(Some(beat)) if passed_over.is_some() => skip(format!(
            "no handoff is on origin, and builder {} passed this release's request over: {}",
            beat.alive.builder,
            passed_over.map_or("", aterm_winship::pull::PassedOver::words)
        )),
        Ok(Some(beat)) if now >= deadline => skip(format!(
            "no handoff is on origin {} s after the request, the wait {WAIT_KEY} allows \
             ({} s); builder {} is still serving",
            now.saturating_sub(pull.requested_at),
            config.wait,
            beat.alive.builder
        )),
        Ok(Some(beat)) => Decision::Wait {
            until: deadline,
            why: format!(
                "builder {} is serving (heartbeat {} s ago) and has not handed off build {} \
                 yet, {} s after the request",
                beat.alive.builder,
                beat.age(now),
                pull.build,
                now.saturating_sub(pull.requested_at)
            ),
        },
    }
}

#[cfg(all(test, unix))]
#[path = "windows_pull_tests.rs"]
mod tests;
