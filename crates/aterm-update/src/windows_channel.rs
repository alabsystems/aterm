// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows: the signed release channel's portable zip, verified, unpacked and staged for the
//! `install.ps1` it carries.
//!
//! A check ([`check`]) reads the channel's newest release and walks the chain the macOS and Linux
//! updaters walk, refusing at the first link that does not hold and staging nothing then:
//!
//! 1. the machine roster (`aterm-machines.toml`) verifies under the pinned paper master
//!    ([`aterm_update_core::pins::PAPER_MASTER_PUBKEYS`]), is at or above the roster sequence
//!    this lane has seen ([`State::roster_floor`]) and is not past its `valid_until`;
//! 2. the appcast (`aterm-appcast.toml`) verifies under a live machine of that roster, names that
//!    machine and a roster sequence no newer than the roster's, and is the release of the tag it
//!    was read from;
//! 3. its Windows zip ([`aterm_update_core::Manifest::windows_artifact`]) is newer than every build
//!    on this machine and not below the signed minimum build, and the downloaded bytes have the
//!    size and SHA-256 the appcast signs;
//! 4. the zip is the portable folder ([`WindowsTarget::validate_zip`]); unpacked into a fresh
//!    folder, its two images pass their PE checks again from disk, it carries `install.ps1`, and
//!    its `BUILD.json` names the signed build;
//! 5. Authenticode signs both images as [`Pins::publisher`], only when a publisher is pinned.
//!    [`aterm_update_core::pins::WINDOWS_SIGNING_PUBLISHER`] is empty in this build (no Windows
//!    signing identity exists yet), so the chain ends at step 4 and the zip's signed SHA-256 is
//!    what binds the images to the release.
//!
//! The newest release need not carry a Windows zip: the release pump publishes Mac-only releases
//! with no person present, each GitHub's `latest`, and only a cut a person runs on a Windows
//! host's handoff carries one. So when the newest release's appcast, verified through steps 1
//! and 2 and so raising every floor it carries, signs no zip for this machine, the check walks
//! back ([`walk_back`]) to the newest earlier release that lists one, and holds it to the same
//! chain under those floors: its appcast verifies under the newest release's admitted roster,
//! never its own older one, from a machine no admitted roster revoked; it is the release of its
//! tag; its build is not below the signed minimum build, which its own minimum build raises too.
//! A Mac-only release therefore leaves every Windows copy on the last Windows release instead of
//! stranding it, and a roster or minimum build it carries still binds that release. The walk reads
//! the channel's releases listing a page of 100 at a time, as far back as GitHub lists (its first
//! 1000 releases, the atpkg index's and the prereleases included: [`WALK_BACK_PAGES`]); only when
//! no release it looks at carries a zip does the check come to [`Checked::NoAsset`].
//!
//! The verified folder is staged under this lane's root, a receipt of every unpacked file's size
//! and SHA-256 is written beside it, and the lane's pointer names it last, with the zip's signed
//! SHA-256 and the machine that signed it. The floors a later check raises judge the stage too:
//! a roster that revokes its signer, or a signed minimum build above it, withdraws it, whatever
//! that check comes to, and until then [`Lane::read_pointer`] and [`Lane::verify_stage`] refuse
//! it. A stage stands as the stage of a release only when that release signs the same zip from
//! the same machine; otherwise it is downloaded and staged again.
//! [`crate::windows::look_with`] weighs that build against the one the local build loop staged,
//! and [`crate::windows::apply`] reads the folder back against its receipt
//! ([`Lane::verify_stage`]) before it runs the folder's `install.ps1`. That re-read catches a
//! folder that changed since it was verified (a disk fault, another tool, a half-removed stage);
//! a process of this user could as well rewrite the installed copy itself, which no updater
//! check can stop.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_update_core::release_asset::{Asset, AssetError, Reader};
use aterm_update_core::roster::{ROSTER_ASSET, ROSTER_SIG_ASSET, Roster};
use aterm_update_core::tag::{TagKind, parse_release_tag};
use aterm_update_core::windows::{
    CONSOLE_IMAGE, PE_HEADER_PREFIX, Subsystem, WINDOWED_IMAGE, WindowsArtifact, WindowsTarget,
    ZIP_ROOT,
};
use aterm_update_core::{FileLock, Manifest, Source};
use serde::{Deserialize, Serialize};

use crate::floor_sources::FloorSources;
use crate::windows::{Smoke, StagedBuild};

/// The channel's signed manifest, read from the newest release (and, when that one carries no
/// zip for this machine, from the newest release before it that does).
pub const APPCAST: &str = "aterm-appcast.toml";
/// The appcast's detached signature, beside it.
pub const APPCAST_SIG: &str = "aterm-appcast.toml.sig";

/// How long after one check the next automatic one is due: the Linux lane's half hour.
pub const CHECK_EVERY: Duration = Duration::from_secs(30 * 60);

/// The most bytes a portable zip may unpack to.
pub const UNPACKED_MAX_BYTES: u64 = 1024 * 1024 * 1024;

/// The page size of the channel's releases listing a check reads ([`Reader::releases`], one
/// request a page) when the newest release carries no zip for this machine ([`walk_back`]):
/// GitHub's largest.
const WALK_BACK_PAGE: u8 = aterm_update_core::release_asset::RELEASES_PAGE_MAX;
/// The most pages of it the walk reads: GitHub lists no release past the first 1000, so the walk
/// looks at most that far back (counting the atpkg index's releases and the prereleases the
/// channel's repository also holds), however many Mac-only releases were cut since the last
/// Windows one. It reads the first page that holds a release it can take and one more, and stops
/// earlier at the listing's last page.
const WALK_BACK_PAGES: u32 = 10;

const APPCAST_LIMIT: u64 = 5 * 1024 * 1024;
const SIG_LIMIT: u64 = 4096;
const ROSTER_LIMIT: u64 = 64 * 1024;
/// The bound on this lane's own records (its state, pointer and receipts).
const RECORD_LIMIT: u64 = 1024 * 1024;

/// How long a check waits for another process's check of the same lane before it says so.
const LOCK_WAIT: Duration = Duration::from_secs(5);

/// The lane's pointer to the build it staged.
const POINTER: &str = "latest.json";
/// The lane's floors and its last check.
const STATE: &str = "state.json";
/// One check at a time.
const LOCK: &str = "channel.lock";
/// A staged folder's receipt sits beside it as `<commit>.receipt.json`.
const RECEIPT_SUFFIX: &str = ".receipt.json";
/// What a download and an unpack are named while they are in flight; a check sweeps them.
const DOWNLOAD_PREFIX: &str = "download-";
const PARTIAL_MARK: &str = ".partial-";

/// The installer the staged folder must carry: the hand-off.
pub const INSTALLER: &str = "install.ps1";
/// The build record every portable folder carries and `install.ps1` lays beside the images.
pub const BUILD_RECORD: &str = "BUILD.json";

/// The trust anchors a check verifies under.
#[derive(Clone, Copy, Debug)]
pub struct Pins<'a> {
    /// The paper master's public keys: the roster must verify under one of them.
    pub masters: &'a [&'a str],
    /// The Authenticode publisher both images must be signed by; empty checks no signature.
    pub publisher: &'a str,
}

/// The committed anchors. Nothing at run time replaces them; tests pass their own [`Pins`].
pub const PRODUCTION_PINS: Pins<'static> = Pins {
    masters: aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
    publisher: aterm_update_core::pins::WINDOWS_SIGNING_PUBLISHER,
};

/// Reads an image's Authenticode signer: the signing certificate's name when the signature
/// verifies, or why it does not. [`authenticode_signer`] in production; a test passes its own.
pub type Signer<'a> = &'a dyn Fn(&Path) -> Result<String, String>;

/// What the machine already has, which a release must beat to be fetched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Have {
    /// The installed copy's build (its `BUILD.json`, else the running image's).
    pub installed: u64,
    /// The build the local build loop offers (0 for none, or one that failed its smoke test).
    /// A release that does not beat it would lose to it ([`crate::windows::pick`]), so it is
    /// not downloaded.
    pub offered: u64,
}

/// What one check came to, when nothing refused it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Checked {
    /// The newest release carries nothing for this copy, nor does any release before it the
    /// walk back looked at, and why.
    NoAsset {
        /// The newest release.
        tag: String,
        /// What it lacks, and what the walk back looked at.
        why: String,
    },
    /// The newest release for this copy's build is not newer than what this machine has or
    /// offers.
    NotNewer {
        /// The release read: the newest, or the newest before it with this machine's zip.
        tag: String,
        /// Its build.
        build: u64,
        /// The newest build on this machine.
        have: u64,
    },
    /// The newest release is staged already, and its folder still matches its receipt.
    AlreadyStaged(StagedBuild),
    /// The newest release was verified, unpacked and staged.
    Staged(StagedBuild),
}

impl Checked {
    /// The check's one line, as the lane records it and `aterm update check` prints it.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Self::NoAsset { tag, why } => format!("the newest release, {tag}, {why}"),
            Self::NotNewer { tag, build, have } => format!(
                "the newest release for this machine, {tag} (build {build}), is not newer than \
                 build {have} on it"
            ),
            Self::AlreadyStaged(staged) => format!("{} is staged and verified", staged.name()),
            Self::Staged(staged) => format!(
                "{} was downloaded from the release channel, verified and staged",
                staged.name()
            ),
        }
    }
}

/// The lane's durable record: the floors no later check goes below, and its last check.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// The highest roster sequence an admitted roster carried: an older roster is a replay.
    #[serde(default)]
    pub roster_floor: u64,
    /// Machines an admitted roster withdrew: their appcasts are refused from then on.
    #[serde(default)]
    pub revoked_machines: Vec<String>,
    /// The signed minimum build, raised by appcasts and lowered only by a revocation of the
    /// machine that raised it ([`FloorSources`]).
    #[serde(default)]
    pub min_build: u64,
    #[serde(default)]
    pub min_build_sources: FloorSources,
    /// When the last check ended (unix seconds; 0 before the first).
    #[serde(default)]
    pub checked_unix: i64,
    /// The same, RFC3339.
    #[serde(default)]
    pub checked_at: String,
    /// What the last check came to, in one line.
    #[serde(default)]
    pub outcome: String,
}

/// The lane's pointer to the folder it staged. Its folder is never read from the file: it is
/// the lane's folder for the commit ([`Lane::dist`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Pointer {
    schema: u32,
    version: String,
    commit: String,
    commit_full: String,
    build_number: u64,
    staged_at: String,
    tag: String,
    /// The zip's SHA-256 as the appcast signed it.
    zip_sha256: String,
    /// The rostered machine whose signature the appcast carried: a roster that revokes it
    /// withdraws the stage ([`admissible`]).
    machine_id: String,
}

/// Whether the lane's floors still admit the stage `pointer` names: its signer is not a machine
/// an admitted roster revoked, and its build is not below the signed minimum build. A stage that
/// was verified under floors a later check raised is withdrawn, never offered or installed.
fn admissible(state: &State, pointer: &Pointer) -> Result<(), String> {
    let name = || {
        format!(
            "aterm {} (commit {}, build {})",
            pointer.version, pointer.commit, pointer.build_number
        )
    };
    if state.revoked_machines.contains(&pointer.machine_id) {
        return Err(format!(
            "the staged {} was signed by machine {}, which the machine roster revoked",
            name(),
            pointer.machine_id
        ));
    }
    if pointer.build_number < state.min_build {
        return Err(format!(
            "the staged {} is below the signed minimum build {}",
            name(),
            state.min_build
        ));
    }
    Ok(())
}

/// One unpacked file, as it was verified.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ReceiptFile {
    /// Its path under the staged folder, `/`-separated.
    path: String,
    size: u64,
    sha256: String,
}

/// What a staged folder held when it was verified.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Receipt {
    schema: u32,
    build_number: u64,
    commit: String,
    zip_sha256: String,
    /// The folders the zip named, `/`-separated (every file's own folders are implied).
    folders: Vec<String>,
    files: Vec<ReceiptFile>,
}

/// A signed document as read from one release (its appcast, or its roster): its bytes and their
/// detached signature.
struct SignedBytes {
    bytes: Vec<u8>,
    signature: Vec<u8>,
}

/// Where this lane keeps its stage and records: `%LOCALAPPDATA%\aterm\update\channel` for a
/// user, any folder for a test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lane {
    root: PathBuf,
}

impl Lane {
    /// The lane rooted at `root`.
    #[must_use]
    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    /// The lane under a `%LOCALAPPDATA%` folder. It is apart from `aterm\dist`, which the local
    /// build loop owns and prunes.
    #[must_use]
    pub fn under(local_app_data: &Path) -> Self {
        Self::at(local_app_data.join("aterm").join("update").join("channel"))
    }

    /// This user's lane: `None` when `%LOCALAPPDATA%` is not set.
    #[cfg(windows)]
    #[must_use]
    pub fn of_this_user() -> Option<Self> {
        std::env::var_os("LOCALAPPDATA")
            .filter(|dir| !dir.is_empty())
            .map(|dir| Self::under(Path::new(&dir)))
    }

    /// The lane's folder.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The pointer [`crate::windows::look_with`] reads.
    #[must_use]
    pub fn pointer(&self) -> PathBuf {
        self.root.join(POINTER)
    }

    /// The staged folder of the build of `commit` (12 hex).
    #[must_use]
    pub fn dist(&self, commit: &str) -> PathBuf {
        self.root.join(commit)
    }

    fn receipt_path(&self, commit: &str) -> PathBuf {
        self.root.join(format!("{commit}{RECEIPT_SUFFIX}"))
    }

    /// Whether `dir` is a staged folder of this lane: a folder of the lane's root named by a
    /// commit.
    #[must_use]
    pub fn holds(&self, dir: &Path) -> bool {
        let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        };
        dir.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_commit)
            && dir.parent().is_some_and(|parent| same(parent, &self.root))
    }

    /// The build the lane staged and its floors still admit ([`admissible`]): `Ok(None)` when it
    /// staged none, an error that says why when its record is unreadable or a later check's
    /// floors withdrew it.
    pub fn read_pointer(&self) -> Result<Option<StagedBuild>, String> {
        let Some(pointer) = self.read_pointer_record()? else {
            return Ok(None);
        };
        admissible(&self.load()?, &pointer)?;
        Ok(Some(self.staged_build(pointer)))
    }

    /// The pointer as written, its floors unread: `Ok(None)` when there is none.
    fn read_pointer_record(&self) -> Result<Option<Pointer>, String> {
        let bytes = match read_bounded(&self.pointer(), RECORD_LIMIT) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("{}: {error}", self.pointer().display())),
        };
        let pointer: Pointer = aterm_json::from_slice(&bytes).map_err(|e| {
            format!(
                "{} is not this lane's pointer: {e}",
                self.pointer().display()
            )
        })?;
        if pointer.schema != 1
            || !is_commit(&pointer.commit)
            || pointer.build_number == 0
            || !pointer.commit_full.starts_with(&pointer.commit)
            || pointer.machine_id.is_empty()
            || !aterm_codec::hex::is_lower(&pointer.zip_sha256, 64)
        {
            return Err(format!(
                "{} names no build this lane stages",
                self.pointer().display()
            ));
        }
        Ok(Some(pointer))
    }

    fn staged_build(&self, pointer: Pointer) -> StagedBuild {
        StagedBuild {
            dir: self.dist(&pointer.commit),
            version: pointer.version,
            commit: pointer.commit,
            build_number: pointer.build_number,
            staged_at: pointer.staged_at,
            smoke: Smoke::NotRun,
            dirty: false,
        }
    }

    /// The lane's record: the default before its first check.
    pub fn load(&self) -> Result<State, String> {
        match read_bounded(&self.root.join(STATE), RECORD_LIMIT) {
            Ok(bytes) => aterm_json::from_slice(&bytes).map_err(|e| {
                format!(
                    "the release channel's record {} is unreadable: {e}",
                    self.root.join(STATE).display()
                )
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(error) => Err(format!(
                "the release channel's record {} is unreadable: {error}",
                self.root.join(STATE).display()
            )),
        }
    }

    /// Hold the lane still: one check at a time, and no check while a staged folder is read back
    /// and installed (a check that staged a newer build would prune it under its installer).
    /// Waits [`LOCK_WAIT`] for another holder, then says so.
    pub fn hold(&self) -> Result<FileLock, String> {
        let lock = self.root.join(LOCK);
        FileLock::acquire_within(&lock, LOCK_WAIT).map_err(|e| {
            if e.kind() == std::io::ErrorKind::TimedOut {
                "another aterm is checking or installing the release channel's build right now"
                    .to_string()
            } else {
                format!("cannot lock {}: {e}", lock.display())
            }
        })
    }

    fn save(&self, state: &State) -> Result<(), String> {
        let text = aterm_json::to_string(state).map_err(|e| e.to_string())?;
        write_atomic(&self.root.join(STATE), text.as_bytes())
    }

    /// Whether an automatic check is due at `now_unix`: never checked, [`CHECK_EVERY`] since the
    /// last, a clock that stepped back past it, or a record that cannot be read (the check then
    /// says why).
    #[must_use]
    pub fn due(&self, now_unix: i64) -> bool {
        self.load().map_or(true, |state| {
            let every = i64::try_from(CHECK_EVERY.as_secs()).unwrap_or(i64::MAX);
            state.checked_unix == 0
                || now_unix < state.checked_unix
                || now_unix >= state.checked_unix.saturating_add(every)
        })
    }

    /// The lane's last check, for the status lines: `automatic` is whether this copy checks by
    /// itself.
    #[must_use]
    pub fn note(&self, automatic: bool) -> crate::windows::ChannelNote {
        let (checked_at, outcome) = match self.load() {
            Ok(state) => (state.checked_at, state.outcome),
            Err(why) => (String::new(), why),
        };
        crate::windows::ChannelNote {
            automatic,
            checked_at,
            outcome,
        }
    }

    /// Read `staged`'s folder back before its installer runs: it must still be the build the
    /// lane's pointer names, which the lane's floors (as last recorded) still admit; then the
    /// folder against its receipt (see [`Lane::verify_stage_under`]).
    pub fn verify_stage(
        &self,
        staged: &StagedBuild,
        publisher: &str,
        signer: Signer<'_>,
    ) -> Result<(), String> {
        self.verify_stage_under(&self.load()?, staged, publisher, signer)
    }

    /// [`Lane::verify_stage`] under the floors `state` holds: the pointer names `staged` and
    /// [`admissible`] admits it; the receipt is of that stage and of the zip the pointer names;
    /// the folder holds every file the verified zip held, of the size and SHA-256 it had, and
    /// nothing else; then its images' Authenticode signer, when a publisher is pinned.
    fn verify_stage_under(
        &self,
        state: &State,
        staged: &StagedBuild,
        publisher: &str,
        signer: Signer<'_>,
    ) -> Result<(), String> {
        let pointer = self.read_pointer_record()?.ok_or_else(|| {
            format!(
                "the release channel no longer stages {} (its pointer is gone)",
                staged.name()
            )
        })?;
        if pointer.commit != staged.commit || pointer.build_number != staged.build_number {
            return Err(format!(
                "the release channel stages aterm {} (commit {}, build {}) now, not {}",
                pointer.version,
                pointer.commit,
                pointer.build_number,
                staged.name()
            ));
        }
        admissible(state, &pointer)?;
        let bytes = read_bounded(&self.receipt_path(&staged.commit), RECORD_LIMIT)
            .map_err(|e| format!("the staged release has no readable receipt: {e}"))?;
        let receipt: Receipt = aterm_json::from_slice(&bytes)
            .map_err(|e| format!("the staged release's receipt is unreadable: {e}"))?;
        if receipt.schema != 1
            || receipt.build_number != staged.build_number
            || receipt.commit != staged.commit
            || receipt.zip_sha256 != pointer.zip_sha256
            || staged.dir != self.dist(&staged.commit)
        {
            return Err(format!(
                "the staged release's receipt does not name {}",
                staged.name()
            ));
        }
        let mut found_files = BTreeMap::new();
        let mut found_folders = BTreeSet::new();
        walk(&staged.dir, "", &mut found_files, &mut found_folders)?;
        let mut folders: BTreeSet<String> = receipt.folders.iter().cloned().collect();
        for file in &receipt.files {
            let mut parent = file.path.as_str();
            while let Some((up, _)) = parent.rsplit_once('/') {
                folders.insert(up.to_string());
                parent = up;
            }
            let Some(path) = found_files.remove(&file.path) else {
                return Err(format!(
                    "the staged {} is gone since it was verified",
                    file.path
                ));
            };
            let (size, sha256) = hash_file(&path)?;
            if size != file.size || sha256 != file.sha256 {
                return Err(format!(
                    "the staged {} changed since it was verified",
                    file.path
                ));
            }
        }
        if let Some(extra) = found_files.keys().next() {
            return Err(format!(
                "the staged folder holds {extra}, which the verified zip did not"
            ));
        }
        if let Some(extra) = found_folders.difference(&folders).next() {
            return Err(format!(
                "the staged folder holds the folder {extra}, which the verified zip did not"
            ));
        }
        check_publisher(&staged.dir, publisher, signer)
    }

    /// Remove what an interrupted check left: downloads and unpacks in flight.
    fn sweep_in_flight(&self) {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let path = entry.path();
            if name.starts_with(DOWNLOAD_PREFIX) {
                let _ = fs::remove_file(&path);
            } else if name.contains(PARTIAL_MARK) {
                let _ = fs::remove_dir_all(&path);
            }
        }
    }

    /// A fresh name in the lane's root for something in flight.
    fn in_flight(&self, stem: &str, suffix: &str) -> Result<PathBuf, String> {
        let token = aterm_uds::rand::hex_token::<8>()
            .map_err(|e| format!("no entropy for a scratch name: {e}"))?;
        Ok(self.root.join(format!("{stem}{token}{suffix}")))
    }

    /// Withdraw a stage the floors in `state` no longer admit ([`admissible`]): its pointer
    /// goes, so no window offers it, and its folder and receipt with it.
    fn withdraw_inadmissible(&self, state: &State) {
        if let Ok(Some(pointer)) = self.read_pointer_record()
            && let Err(why) = admissible(state, &pointer)
        {
            aterm_log::info!("aterm-update: withdrawing the release channel's stage: {why}");
            let _ = fs::remove_file(self.pointer());
            self.prune(None);
        }
    }

    /// Remove the pointer when it names `commit`, and only then: a pointer that names another
    /// stage keeps offering it.
    fn withdraw_if_names(&self, commit: &str) {
        if self
            .read_pointer_record()
            .is_ok_and(|pointer| pointer.is_some_and(|pointer| pointer.commit == commit))
        {
            let _ = fs::remove_file(self.pointer());
        }
    }

    /// Unpack the verified `zip` into a fresh folder, check what it holds, and stage it as the
    /// lane's build: the folder renamed into place, its receipt beside it, the pointer last.
    #[expect(
        clippy::too_many_arguments,
        reason = "what was verified so far, and the publisher seam"
    )]
    fn stage(
        &self,
        zip: &Path,
        signed: &Signed,
        artifact: &WindowsArtifact<'_>,
        commit_full: &str,
        tag: &str,
        publisher: &str,
        signer: Signer<'_>,
        now_unix: i64,
    ) -> Result<StagedBuild, String> {
        let manifest = &signed.manifest;
        let commit = &commit_full[..12];
        let partial = self.in_flight(&format!("{commit}{PARTIAL_MARK}"), "")?;
        fs::create_dir(&partial).map_err(|e| format!("cannot make {}: {e}", partial.display()))?;
        let checked = unpack(zip, &partial, artifact.target).and_then(|(folders, files)| {
            check_unpacked(&partial, manifest, commit, artifact.target)?;
            check_publisher(&partial, publisher, signer)?;
            Ok((folders, files))
        });
        let (folders, files) = match checked {
            Ok(unpacked) => unpacked,
            Err(why) => {
                let _ = fs::remove_dir_all(&partial);
                return Err(why);
            }
        };
        let dest = self.dist(commit);
        // The pointer must not name a folder while it is replaced.
        self.withdraw_if_names(commit);
        if dest.exists() {
            fs::remove_dir_all(&dest).map_err(|e| {
                let _ = fs::remove_dir_all(&partial);
                format!("cannot replace {}: {e}", dest.display())
            })?;
        }
        fs::rename(&partial, &dest).map_err(|e| {
            let _ = fs::remove_dir_all(&partial);
            format!("cannot move the verified folder to {}: {e}", dest.display())
        })?;
        let receipt = Receipt {
            schema: 1,
            build_number: manifest.build_number,
            commit: commit.to_string(),
            zip_sha256: artifact.sha256.to_string(),
            folders,
            files,
        };
        let staged_at = rfc3339(now_unix);
        let pointer = Pointer {
            schema: 1,
            version: manifest.version.clone(),
            commit: commit.to_string(),
            commit_full: commit_full.to_string(),
            build_number: manifest.build_number,
            staged_at: staged_at.clone(),
            tag: tag.to_string(),
            zip_sha256: artifact.sha256.to_string(),
            machine_id: signed.machine_id.clone(),
        };
        let written = aterm_json::to_string(&receipt)
            .map_err(|e| e.to_string())
            .and_then(|text| write_atomic(&self.receipt_path(commit), text.as_bytes()))
            .and_then(|()| aterm_json::to_string(&pointer).map_err(|e| e.to_string()))
            .and_then(|text| write_atomic(&self.pointer(), text.as_bytes()));
        if let Err(why) = written {
            // A pointer that still names the stage before this one keeps offering it: the
            // pointer is written whole or not at all, and only after the receipt.
            self.withdraw_if_names(commit);
            let _ = fs::remove_dir_all(&dest);
            let _ = fs::remove_file(self.receipt_path(commit));
            return Err(why);
        }
        self.prune(Some(commit));
        Ok(StagedBuild {
            version: manifest.version.clone(),
            commit: commit.to_string(),
            build_number: manifest.build_number,
            dir: dest,
            staged_at,
            smoke: Smoke::NotRun,
            dirty: false,
        })
    }

    /// Remove every stage but `keep`'s (every one, for `None`): folders named by a commit and
    /// their receipts, nothing else. A folder a running image holds stays until a later check.
    fn prune(&self, keep: Option<&str>) {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if is_commit(name) && Some(name) != keep {
                let _ = fs::remove_dir_all(entry.path());
            } else if let Some(commit) = name.strip_suffix(RECEIPT_SUFFIX)
                && is_commit(commit)
                && Some(commit) != keep
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// Whether `name` is a staged folder's name: a commit's first twelve lowercase hex digits.
fn is_commit(name: &str) -> bool {
    name.len() == 12 && aterm_codec::hex::is_lower(name, 12)
}

fn rfc3339(unix: i64) -> String {
    aterm_types::rfc3339::format_rfc3339(u64::try_from(unix).unwrap_or(0))
}

fn read_bounded(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_or(true, |len| len > limit) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("larger than {limit} bytes"),
        ));
    }
    Ok(bytes)
}

/// Write `bytes` to `path` through a sibling written and flushed whole, then renamed over it.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no file name", path.display()))?;
    let token = aterm_uds::rand::hex_token::<8>()
        .map_err(|e| format!("no entropy for a scratch name: {e}"))?;
    let temp = path.with_file_name(format!("{DOWNLOAD_PREFIX}{name}-{token}"));
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            aterm_durable::durable(&file)
        })
        .and_then(|()| fs::rename(&temp, path));
    written.map_err(|e| {
        let _ = fs::remove_file(&temp);
        format!("cannot write {}: {e}", path.display())
    })
}

/// A file's size and lowercase SHA-256.
fn hash_file(path: &Path) -> Result<(u64, String), String> {
    let file = File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    hash_reader(file).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

fn hash_reader(mut reader: impl Read) -> std::io::Result<(u64, String)> {
    let mut hash = aterm_digest::Sha256::new();
    let mut size = 0u64;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size = size.saturating_add(count as u64);
        hash.update(&buffer[..count]);
    }
    Ok((size, aterm_codec::hex::encode_lower(&hash.finalize())))
}

/// Every file (by its `/`-separated path under `dir`) and folder below `dir`; anything that is
/// neither, a link or a junction included, refuses.
fn walk(
    dir: &Path,
    prefix: &str,
    files: &mut BTreeMap<String, PathBuf>,
    folders: &mut BTreeSet<String>,
) -> Result<(), String> {
    let entries =
        fs::read_dir(dir).map_err(|e| format!("cannot read the staged {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read the staged {}: {e}", dir.display()))?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| format!("the staged folder holds a name that is not UTF-8: {name:?}"))?;
        let path = entry.path();
        let relative = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        let kind = fs::symlink_metadata(&path)
            .map_err(|e| format!("cannot read the staged {relative}: {e}"))?
            .file_type();
        if kind.is_symlink() {
            return Err(format!("the staged {relative} is a link"));
        } else if kind.is_dir() {
            walk(&path, &relative, files, folders)?;
            folders.insert(relative);
        } else if kind.is_file() {
            files.insert(relative, path);
        } else {
            return Err(format!(
                "the staged {relative} is neither a file nor a folder"
            ));
        }
    }
    Ok(())
}

/// Unpack every member of the portable zip at `zip` under `into`, its top-level folder dropped,
/// and say what was written: the folders the zip named and each file's size and SHA-256.
fn unpack(
    zip: &Path,
    into: &Path,
    target: WindowsTarget,
) -> Result<(Vec<String>, Vec<ReceiptFile>), String> {
    let unreadable = |e: std::io::Error| format!("the zip is unreadable: {e}");
    target.validate_zip(File::open(zip).map_err(unreadable)?)?;
    let mut archive = aterm_codec::zip::Archive::open(File::open(zip).map_err(unreadable)?)
        .map_err(unreadable)?;
    let mut folders = Vec::new();
    let mut files = Vec::new();
    let mut total = 0u64;
    while let Some(entry) = archive.next_entry().map_err(unreadable)? {
        let (path, folder) = aterm_update_core::windows::member_path(&entry.name)?;
        let Some(relative) = path
            .strip_prefix(ZIP_ROOT)
            .map(|rest| rest.trim_end_matches('/'))
            .filter(|rest| !rest.is_empty())
        else {
            continue;
        };
        let out = relative
            .split('/')
            .fold(into.to_path_buf(), |at, part| at.join(part));
        let make = |dir: &Path| {
            fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))
        };
        if folder {
            make(&out)?;
            folders.push(relative.to_string());
            continue;
        }
        if let Some(parent) = out.parent() {
            make(parent)?;
        }
        total = total.saturating_add(entry.uncomp_size);
        if total > UNPACKED_MAX_BYTES {
            return Err(format!(
                "the zip unpacks to more than {UNPACKED_MAX_BYTES} bytes"
            ));
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&out)
            .map_err(|e| format!("cannot write {}: {e}", out.display()))?;
        let body = archive.body(&entry).map_err(unreadable)?;
        let mut hash = aterm_digest::Sha256::new();
        let mut size = 0u64;
        let mut body = body.take(entry.uncomp_size.saturating_add(1));
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let count = body.read(&mut buffer).map_err(unreadable)?;
            if count == 0 {
                break;
            }
            size = size.saturating_add(count as u64);
            hash.update(&buffer[..count]);
            file.write_all(&buffer[..count])
                .map_err(|e| format!("cannot write {}: {e}", out.display()))?;
        }
        if size != entry.uncomp_size {
            return Err(format!(
                "the zip's {path} unpacks to {size} bytes, not the {} it declares",
                entry.uncomp_size
            ));
        }
        aterm_durable::durable(&file)
            .map_err(|e| format!("cannot write {}: {e}", out.display()))?;
        files.push(ReceiptFile {
            path: relative.to_string(),
            size,
            sha256: aterm_codec::hex::encode_lower(&hash.finalize()),
        });
    }
    Ok((folders, files))
}

/// What the unpacked folder must hold: both images, as their PE headers say on disk, the
/// installer, and a build record naming the signed build.
fn check_unpacked(
    dir: &Path,
    manifest: &Manifest,
    commit: &str,
    target: WindowsTarget,
) -> Result<(), String> {
    for (member, subsystem) in [
        (CONSOLE_IMAGE, Subsystem::Console),
        (WINDOWED_IMAGE, Subsystem::Windowed),
    ] {
        let name = &member[ZIP_ROOT.len()..];
        let mut head = Vec::new();
        File::open(dir.join(name))
            .and_then(|file| file.take(PE_HEADER_PREFIX).read_to_end(&mut head))
            .map_err(|e| format!("the unpacked {name} is unreadable: {e}"))?;
        target
            .validate_pe_header(&head, subsystem)
            .map_err(|why| format!("the unpacked {name}: {why}"))?;
    }
    if !dir.join(INSTALLER).is_file() {
        return Err(format!("the zip carries no {INSTALLER} to install it with"));
    }
    let text = read_bounded(&dir.join(BUILD_RECORD), RECORD_LIMIT)
        .map_err(|e| format!("the zip carries no readable {BUILD_RECORD}: {e}"))?;
    let record = crate::windows::parse_installed(&String::from_utf8_lossy(&text))
        .ok_or_else(|| format!("the zip's {BUILD_RECORD} names no build"))?;
    if record.build_number != manifest.build_number
        || record.commit != commit
        || record.version != manifest.version
        || record.dirty
    {
        return Err(format!(
            "the zip's {BUILD_RECORD} names {}, not the signed aterm {} (commit {commit}, build {})",
            record.name(),
            manifest.version,
            manifest.build_number
        ));
    }
    Ok(())
}

/// Both images' Authenticode signer must be `publisher`. An empty `publisher` checks nothing:
/// no publisher is pinned ([`aterm_update_core::pins::WINDOWS_SIGNING_PUBLISHER`] is empty), so
/// the signed SHA-256 alone binds the images, and `signer` is never asked.
fn check_publisher(dir: &Path, publisher: &str, signer: Signer<'_>) -> Result<(), String> {
    if !aterm_update_core::pins::anchor_active(publisher) {
        return Ok(());
    }
    for member in [CONSOLE_IMAGE, WINDOWED_IMAGE] {
        let name = &member[ZIP_ROOT.len()..];
        match signer(&dir.join(name)) {
            Ok(who) if who == publisher => {}
            Ok(who) => {
                return Err(format!(
                    "{name} is signed by {who:?}, not the pinned publisher {publisher:?}"
                ));
            }
            Err(why) => return Err(format!("{name}'s Authenticode signature refused: {why}")),
        }
    }
    Ok(())
}

/// An appcast that verified, and the rostered machine whose signature it carried.
struct Signed {
    manifest: Manifest,
    machine_id: String,
}

/// Verify the roster under the pinned master and admit it, raising the lane's floors as the
/// macOS and Linux updaters raise theirs: the roster sequence, the revocations, and the signed
/// minimum build a revocation takes back ([`FloorSources`]). The admitted roster is what every
/// appcast of this check is authorized under ([`authorize_appcast`]).
fn admit_roster(
    state: &mut State,
    signed: &SignedBytes,
    pins: Pins<'_>,
    now_unix: i64,
) -> Result<Roster, String> {
    if pins.masters.is_empty() {
        return Err("this build pins no paper master, so it verifies no update channel".into());
    }
    let verified =
        aterm_update_core::verify_roster(pins.masters, signed.bytes.clone(), &signed.signature)
            .map_err(|e| format!("machine roster signature refused: {e:?}"))?;
    let roster = Roster::parse(&verified).map_err(|e| format!("machine roster refused: {e:?}"))?;
    roster
        .admit(state.roster_floor, now_unix)
        .map_err(|e| format!("machine roster admission refused: {e:?}"))?;
    state.roster_floor = state.roster_floor.max(roster.roster_seq);
    for machine in &roster.revoked {
        if !state.revoked_machines.contains(machine) {
            state.revoked_machines.push(machine.clone());
        }
    }
    (state.min_build, state.min_build_sources) =
        state
            .min_build_sources
            .step(state.min_build, &state.revoked_machines, 0, None);
    Ok(roster)
}

/// Verify an appcast under `roster`, already admitted ([`admit_roster`]): signed by one of its
/// live machines, which no admitted roster revoked, and naming that machine and a roster
/// sequence no newer than the roster's. Its signed minimum build then raises the lane's floor,
/// attributed to that machine.
fn authorize_appcast(
    state: &mut State,
    roster: &Roster,
    signed: &SignedBytes,
    now_unix: i64,
) -> Result<Signed, String> {
    let attribution = roster
        .authorize_appcast(&signed.bytes, &signed.signature, now_unix)
        .map_err(|e| format!("appcast signature refused: {e:?}"))?;
    let text = std::str::from_utf8(&signed.bytes)
        .map_err(|_| "appcast refused: it is not UTF-8".to_string())?;
    let manifest = Manifest::parse(text).map_err(|e| format!("appcast refused: {e}"))?;
    if state.revoked_machines.contains(&attribution.machine_id) {
        return Err(format!(
            "appcast refused: its signer {} is revoked",
            attribution.machine_id
        ));
    }
    attribution
        .bind(manifest.machine_id.as_deref(), manifest.roster_seq)
        .map_err(|e| format!("appcast signer attribution refused: {e:?}"))?;
    (state.min_build, state.min_build_sources) = state.min_build_sources.step(
        state.min_build,
        &state.revoked_machines,
        manifest.min_build.unwrap_or(0),
        Some(&attribution.machine_id),
    );
    Ok(Signed {
        manifest,
        machine_id: attribution.machine_id,
    })
}

/// An appcast must be the release of the tag it was read from: a `vMAJOR.MINOR.PATCH` tag that
/// is `v` and the version it signs.
fn of_its_tag(tag: &str, manifest: &Manifest) -> Result<(), String> {
    if !matches!(parse_release_tag(tag), Ok(TagKind::Candidate(_)))
        || tag != format!("v{}", manifest.version)
    {
        return Err(format!(
            "appcast refused: it signs aterm {}, not the release {tag} it was read from",
            manifest.version
        ));
    }
    Ok(())
}

/// The full source commit an appcast names, in lowercase, when it names one and a build.
fn source_commit(manifest: &Manifest) -> Result<String, String> {
    manifest
        .commit
        .as_deref()
        .filter(|commit| aterm_codec::hex::is_hex(commit, 40))
        .map(str::to_ascii_lowercase)
        .filter(|_| manifest.build_number != 0)
        .ok_or_else(|| "appcast refused: it names no source commit and build".to_string())
}

/// What [`walk_back`] came to when nothing refused it.
enum Walked {
    /// The release of this tag carries this machine's zip, and its appcast verified.
    Found(String, Box<Signed>),
    /// No release it looked at carries one, and what it looked at.
    Nothing(String),
}

/// Walk back from `latest`, whose signed appcast carries no zip for `target`, to the newest
/// release before it that does, and hold that release to the chain `latest` was held to, under
/// the floors `latest`'s check raised.
///
/// The candidates are the channel's releases as the API lists them ([`Reader::releases`]), newest
/// first by creation, in pages of [`WALK_BACK_PAGE`]: the walk reads pages until the one after
/// the first that holds a candidate carrying this target's zip, the listing's last page, or
/// [`WALK_BACK_PAGES`], whichever comes first. Drafts, prereleases and tags that are not
/// `vMAJOR.MINOR.PATCH` are skipped, and so is every release not older than `latest` by its
/// version. The candidates read are weighed newest version first, never by where the listing
/// puts them, and only the newest whose asset list names this target's zip for its version
/// ([`WindowsTarget::asset_name`], the name the cutter publishes) is read. Its appcast must
/// verify under `roster` (the roster `latest` carried, which this check admitted, never the
/// release's own older one), its signer must not be revoked, it must be the release of its tag
/// ([`of_its_tag`]), name its source commit and build, sign this target's zip, and its build
/// must not be below the signed minimum build; its own signed minimum build raises the floor
/// too. A release that fails a link refuses the check, naming the release and the link: an
/// older release is not tried in its place.
fn walk_back(
    reader: &Reader,
    source: &Source,
    state: &mut State,
    roster: &Roster,
    latest: &str,
    target: WindowsTarget,
    now_unix: i64,
) -> Result<Walked, String> {
    let triple = target.triple();
    let Ok(TagKind::Candidate(latest_key)) = parse_release_tag(latest) else {
        return Err(format!("{latest} is not a release tag to walk back from"));
    };
    // Each earlier release's version key, its tag, and whether it lists this target's zip.
    let mut earlier: Vec<(Vec<u64>, String, bool)> = Vec::new();
    let mut listed = 0_usize;
    let mut pages = 0_u32;
    let mut found_on = None;
    for page in 1..=WALK_BACK_PAGES {
        let releases = reader
            .releases(&source.owner, &source.repo, WALK_BACK_PAGE, page)
            .map_err(|e| {
                format!(
                    "the newest release, {latest}, carries no {triple} zip, and the releases \
                     before it could not be listed (page {page}): {e}"
                )
            })?;
        pages = page;
        listed += releases.len();
        let last = releases.len() < usize::from(WALK_BACK_PAGE);
        for release in releases {
            if release.draft || release.prerelease {
                continue;
            }
            let Ok(TagKind::Candidate(key)) = parse_release_tag(&release.tag) else {
                continue;
            };
            // A release created while the pages were read shifts the listing: the one it
            // pushed down a page is listed twice.
            if key >= latest_key || earlier.iter().any(|(_, tag, _)| *tag == release.tag) {
                continue;
            }
            let Some(version) = aterm_update_core::tag::canonical_version(&release.tag, &key)
            else {
                continue;
            };
            let carries = release.assets.contains(&target.asset_name(&version));
            if carries && found_on.is_none() {
                found_on = Some(page);
            }
            earlier.push((key, release.tag, carries));
        }
        // One page past the first candidate: a release created before it, so listed after it,
        // may still be of a newer version.
        if last || found_on.is_some_and(|found| page > found) {
            break;
        }
    }
    earlier.sort_by(|a, b| b.0.cmp(&a.0));
    let looked: Vec<String> = earlier.iter().map(|(_, tag, _)| tag.clone()).collect();
    let Some((_, tag, _)) = earlier.into_iter().find(|(_, _, carries)| *carries) else {
        const NAMED: usize = 10;
        let looked = match looked.len() {
            0 => "none of them is a published vMAJOR.MINOR.PATCH release older than it".to_string(),
            n if n <= NAMED => format!("looked at {}", looked.join(", ")),
            n => format!(
                "looked at {} and {} older",
                looked[..NAMED].join(", "),
                n - NAMED
            ),
        };
        let pages = if pages == 1 {
            "1 page".to_string()
        } else {
            format!("{pages} pages")
        };
        return Ok(Walked::Nothing(format!(
            "carries no {triple} zip, and no release before it does among the {listed} the \
             channel lists on {pages} of {WALK_BACK_PAGE} ({looked})"
        )));
    };
    let refused = |why: String| {
        format!(
            "the newest release, {latest}, carries no {triple} zip, and the newest before it \
             that does, {tag}, is refused: {why}"
        )
    };
    let read = |name: &str, limit| {
        reader
            .bytes(
                &Asset::tagged(&source.owner, &source.repo, &tag, name),
                limit,
            )
            .map_err(|e| refused(format!("could not read its {name}: {e}")))
    };
    let appcast = SignedBytes {
        bytes: read(APPCAST, APPCAST_LIMIT)?,
        signature: read(APPCAST_SIG, SIG_LIMIT)?,
    };
    let signed = authorize_appcast(state, roster, &appcast, now_unix).map_err(refused)?;
    let manifest = &signed.manifest;
    of_its_tag(&tag, manifest).map_err(refused)?;
    source_commit(manifest).map_err(refused)?;
    if manifest
        .windows_artifact(target)
        .map_err(refused)?
        .is_none()
    {
        return Err(refused(format!(
            "its signed appcast carries no {triple} zip"
        )));
    }
    if manifest.build_number < state.min_build {
        return Err(refused(format!(
            "aterm {} (build {}) is below the signed minimum build {}",
            manifest.version, manifest.build_number, state.min_build
        )));
    }
    Ok(Walked::Found(tag, Box::new(signed)))
}

/// The Windows target this copy installs: the executable's own, and in a test on another
/// platform the one target there is.
fn this_target() -> Option<WindowsTarget> {
    if cfg!(all(test, not(windows))) {
        Some(WindowsTarget::X86_64)
    } else {
        WindowsTarget::native()
    }
}

/// Check the channel's newest release (or, when it carries no zip for this machine, the newest
/// before it that does) and stage it when it verifies and beats `have`: see the module's chain.
/// Every refusal names its link and stages nothing; the lane's floors and its last check's line
/// are recorded whatever the check came to.
pub fn check(
    lane: &Lane,
    reader: &Reader,
    source: &Source,
    pins: Pins<'_>,
    have: Have,
    now_unix: i64,
    signer: Signer<'_>,
) -> Result<Checked, String> {
    fs::create_dir_all(&lane.root)
        .map_err(|e| format!("cannot make {}: {e}", lane.root.display()))?;
    let _held = lane.hold()?;
    let mut state = lane.load()?;
    lane.sweep_in_flight();
    let result = check_locked(
        lane, &mut state, reader, source, pins, have, now_unix, signer,
    );
    state.checked_unix = now_unix;
    state.checked_at = rfc3339(now_unix);
    state.outcome = match &result {
        Ok(checked) => checked.line(),
        Err(why) => why.clone(),
    };
    match &result {
        Ok(checked) => aterm_log::info!("aterm-update: release channel: {}", checked.line()),
        Err(why) => aterm_log::warn!("aterm-update: release channel: {why}"),
    }
    lane.save(&state)?;
    result
}

#[expect(
    clippy::too_many_arguments,
    reason = "the check's inputs, each a seam a test replaces"
)]
fn check_locked(
    lane: &Lane,
    state: &mut State,
    reader: &Reader,
    source: &Source,
    pins: Pins<'_>,
    have: Have,
    now_unix: i64,
    signer: Signer<'_>,
) -> Result<Checked, String> {
    let tag = reader
        .latest_tag(&source.owner, &source.repo, APPCAST)
        .map_err(|e| format!("could not find the newest release: {e}"))?;
    let read = |name: &str, limit| {
        reader.bytes(
            &Asset::tagged(&source.owner, &source.repo, &tag, name),
            limit,
        )
    };
    let appcast = match read(APPCAST, APPCAST_LIMIT) {
        Ok(bytes) => bytes,
        Err(AssetError::Absent(_)) => {
            return Ok(Checked::NoAsset {
                tag,
                why: format!("carries no {APPCAST} yet"),
            });
        }
        Err(error) => return Err(format!("could not read {APPCAST} of {tag}: {error}")),
    };
    let fetch = |name: &str, limit| {
        read(name, limit).map_err(|e| format!("could not read {name} of {tag}: {e}"))
    };
    let appcast = SignedBytes {
        bytes: appcast,
        signature: fetch(APPCAST_SIG, SIG_LIMIT)?,
    };
    let roster = if pins.masters.is_empty() {
        SignedBytes {
            bytes: Vec::new(),
            signature: Vec::new(),
        }
    } else {
        SignedBytes {
            bytes: fetch(ROSTER_ASSET, ROSTER_LIMIT)?,
            signature: fetch(ROSTER_SIG_ASSET, SIG_LIMIT)?,
        }
    };
    let authorized = admit_roster(state, &roster, pins, now_unix).and_then(|roster| {
        let signed = authorize_appcast(state, &roster, &appcast, now_unix)?;
        Ok((roster, signed))
    });
    // The floors an admitted roster raised hold even when its appcast is refused: a stage they
    // no longer admit (its signer revoked, its build below the signed minimum) is withdrawn
    // first, whatever the rest of this check comes to.
    lane.withdraw_inadmissible(state);
    lane.save(state)?;
    let (roster, signed) = authorized?;
    of_its_tag(&tag, &signed.manifest)?;
    let Some(target) = this_target() else {
        return Ok(Checked::NoAsset {
            tag,
            why: "has no Windows build for this machine's architecture".to_string(),
        });
    };
    // A release with no zip for this machine (a Mac-only one) leaves the newest release before
    // it that has one, held to the same chain under the floors this one raised.
    let (tag, signed) = if signed.manifest.windows_artifact(target)?.is_some() {
        (tag, signed)
    } else {
        let walked = walk_back(reader, source, state, &roster, &tag, target, now_unix);
        // Its appcast's signed minimum build holds whatever the walk came to.
        lane.withdraw_inadmissible(state);
        match walked? {
            Walked::Found(tag, signed) => (tag, *signed),
            Walked::Nothing(why) => return Ok(Checked::NoAsset { tag, why }),
        }
    };
    let manifest = &signed.manifest;
    let Some(artifact) = manifest.windows_artifact(target)? else {
        return Err(format!(
            "appcast refused: the appcast of {tag} signs no {} zip",
            target.triple()
        ));
    };
    let commit_full = source_commit(manifest)?;
    if manifest.build_number < state.min_build {
        return Err(format!(
            "aterm {} (build {}) is below the signed minimum build {}: not staged",
            manifest.version, manifest.build_number, state.min_build
        ));
    }
    let newest_here = have.installed.max(have.offered);
    if manifest.build_number <= newest_here {
        return Ok(Checked::NotNewer {
            tag,
            build: manifest.build_number,
            have: newest_here,
        });
    }
    // The stage stands only as the stage of this very release: the same build and commit, the
    // zip this appcast signs, attributed to the machine that signed it (so a roster that later
    // revokes another machine does not withdraw it, and one that revokes this machine does).
    if let Ok(Some(pointer)) = lane.read_pointer_record()
        && pointer.build_number == manifest.build_number
        && pointer.commit_full == commit_full
        && pointer.zip_sha256 == artifact.sha256
        && pointer.machine_id == signed.machine_id
    {
        let staged = lane.staged_build(pointer);
        if lane
            .verify_stage_under(state, &staged, pins.publisher, signer)
            .is_ok()
        {
            return Ok(Checked::AlreadyStaged(staged));
        }
    }
    let zip = lane.in_flight(DOWNLOAD_PREFIX, ".zip")?;
    let staged = reader
        .to_file(
            &Asset::tagged(&source.owner, &source.repo, &tag, artifact.name),
            &zip,
            artifact.size,
        )
        .map_err(|e| format!("could not download {}: {e}", artifact.name))
        .and_then(|()| verify_download(&zip, &artifact))
        .and_then(|()| {
            lane.stage(
                &zip,
                &signed,
                &artifact,
                &commit_full,
                &tag,
                pins.publisher,
                signer,
                now_unix,
            )
        });
    let _ = fs::remove_file(&zip);
    let mut part = zip.into_os_string();
    part.push(".part");
    let _ = fs::remove_file(PathBuf::from(part));
    staged.map(Checked::Staged)
}

/// The downloaded zip's size and SHA-256 must be what the appcast signs.
fn verify_download(zip: &Path, artifact: &WindowsArtifact<'_>) -> Result<(), String> {
    let (size, sha256) = hash_file(zip)?;
    if size != artifact.size {
        return Err(format!(
            "the downloaded {} is {size} bytes, not the {} the signed appcast names",
            artifact.name, artifact.size
        ));
    }
    if sha256 != artifact.sha256 {
        return Err(format!(
            "the downloaded {}'s SHA-256 differs from the signed appcast's",
            artifact.name
        ));
    }
    Ok(())
}

/// Read what `Get-AuthenticodeSignature` printed for one image (`status=<word>` and
/// `signer=<name>` lines): the signer's name when the status is `Valid`, else why not.
pub fn parse_signer(output: &str) -> Result<String, String> {
    let field = |key: &str| {
        output
            .lines()
            .find_map(|line| line.trim().strip_prefix(key))
            .map(str::trim)
    };
    match (field("status="), field("signer=")) {
        (Some("Valid"), Some(signer)) if !signer.is_empty() => Ok(signer.to_string()),
        (Some("Valid"), _) => Err("its signature is valid but names no signer".to_string()),
        (Some(status), _) if !status.is_empty() => Err(format!("its signature is {status}")),
        _ => Err("Get-AuthenticodeSignature said nothing of it".to_string()),
    }
}

/// An image's Authenticode signer as Windows judges it (`Get-AuthenticodeSignature`, which asks
/// `WinVerifyTrust`): the signing certificate's simple name when the signature is valid.
/// Asked only when a publisher is pinned.
///
/// Windows PowerShell finds its own module path: it is not handed this process's
/// `PSModulePath`. PowerShell 7 puts its own builds of Windows PowerShell's modules first in the
/// path its children inherit (`aterm update check` typed at a PowerShell 7 prompt is one), and
/// Windows PowerShell, autoloading `Microsoft.PowerShell.Security` from there, cannot load it,
/// so it would never read a signature.
#[cfg(windows)]
pub fn authenticode_signer(image: &Path) -> Result<String, String> {
    const SCRIPT: &str = "$s = Get-AuthenticodeSignature -LiteralPath $env:ATERM_AUTHENTICODE_IMAGE; \
         $n = ''; if ($s.SignerCertificate) { $n = $s.SignerCertificate.GetNameInfo(\
         [System.Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false) }; \
         [Console]::Out.Write(\"status=$($s.Status)`nsigner=$n`n\")";
    let output = aterm_uds::spawnfd::command(crate::windows::powershell())
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            SCRIPT,
        ])
        .env("ATERM_AUTHENTICODE_IMAGE", image)
        .env_remove("PSModulePath")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("could not start PowerShell to read its signature: {e}"))?;
    parse_signer(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(test)]
#[path = "windows_channel_tests.rs"]
mod tests;
