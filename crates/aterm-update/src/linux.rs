// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Opt-in, signed Linux single-executable updates.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use aterm_update_core::linux::LinuxTarget;

use crate::floor_sources::FloorSources;
use crate::linux_trial::{Lane, LaneConfirm, Lanes, StartClock, Steady};
use aterm_update_core::{FileLock, Manifest, Source};
use serde::{Deserialize, Serialize};

const RECORD_LIMIT: u64 = 256 * 1024;
const APPCAST_LIMIT: u64 = 5 * 1024 * 1024;
const BINARY_LIMIT: u64 = 512 * 1024 * 1024;
const APPCAST: &str = "aterm-appcast.toml";
const APPCAST_SIG: &str = "aterm-appcast.toml.sig";
const ROSTER: &str = "aterm-machines.toml";
const ROSTER_SIG: &str = "aterm-machines.toml.sig";
const POLICY: &str = "aterm-policy-appcast.toml";
const POLICY_SIG: &str = "aterm-policy-appcast.toml.sig";
const MAX_TRIAL_STARTS: u32 = crate::LINUX_TRIAL_LAUNCHES;
/// What a copy that is not enrolled, or whose enrollment never finished, says.
const ENABLE_REMEDY: &str = "Run `aterm update enable` to turn on updates for this copy";
/// What a pass says when another aterm holds this copy's update lock past the wait
/// ([`Context::lock`]): nothing was checked, so it is no failed check
/// ([`crate::linux_notice::Record::Busy`]).
const LOCK_BUSY: &str = "Another aterm is updating this copy right now; try again when it \
    finishes (an `aterm update` command stopped with Ctrl-Z holds it until it is resumed or \
    ended)";
/// What a locked pass says when the record it found enrolled is gone under the lock.
const MISSING_ENROLLMENT: &str = "missing enrollment";

/// A replaced executable waits for one start of it to confirm it ([`confirm`]): a
/// window's or a terminal session's, whichever comes first (a copy used only from
/// the terminal never opens a window).
fn installed_pending(version: &str) -> String {
    format!(
        "aterm {version} is installed \u{2014} the next aterm you start finishes it; existing \
         sessions continue unchanged"
    )
}

/// The refusal while a replaced executable still waits for that start.
fn awaiting_start(state: &State) -> Option<String> {
    state
        .trial
        .as_ref()
        .filter(|trial| trial.phase == Phase::Installed && !trial.healthy)
        .map(|_| {
            format!(
                "aterm {} is installed \u{2014} the next aterm you start finishes it",
                state.installed.version
            )
        })
}

mod ancestry;

/// The install-path rule the installer's embedded verifier is held to (`crates/aterm-update/tests/install_sh`).
#[cfg(any(test, debug_assertions))]
pub mod install_path {
    pub use super::ancestry::refusal;
    pub use aterm_uds::shared_write::{
        Account, Group, REMOTE_FILESYSTEMS, acl_carried, is_user_private_group, remote_filesystem,
        user_private_group,
    };
    /// The seams over what only Linux reads: an access ACL laid, the filesystem type and the passwd enumeration.
    #[cfg(target_os = "linux")]
    pub use aterm_uds::shared_write::{
        filesystem_type_at, grant_group_class_by_acl, primary_members,
    };
}

#[derive(Clone, Debug)]
struct Context {
    target: PathBuf,
    dir: PathBuf,
    /// The held candidate's digest, by the file it was read from
    /// ([`Context::candidate_digest`]).
    candidate_hashed: std::sync::Arc<std::sync::Mutex<Option<(FileStamp, String)>>>,
}

/// Which file, and which bytes of it, a digest was read from. A rename over the file (as
/// every writer of the candidate does) changes its inode, and a length change its size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl FileStamp {
    fn of(md: &fs::Metadata) -> Self {
        Self {
            dev: md.dev(),
            ino: md.ino(),
            size: md.size(),
            mtime: (md.mtime(), md.mtime_nsec()),
            ctime: (md.ctime(), md.ctime_nsec()),
        }
    }
}

/// Capture the canonical executable path before the first replacement. On Linux
/// current_exe() later names the unlinked old inode with a " (deleted)" suffix.
static CONTEXT: OnceLock<Result<Context, String>> = OnceLock::new();

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Identity {
    build: u64,
    sha256: String,
    version: String,
    commit: String,
    machine_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
    Prepared,
    Installed,
    RollbackPrepared,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Trial {
    old: Identity,
    new: Identity,
    phase: Phase,
    starts: u32,
    healthy: bool,
    /// When the latest counted start began (Unix seconds; `0` for none). A start over the
    /// budget rolls back only once this one can no longer confirm itself
    /// ([`crate::linux_trial::start_rolls_back`]): a burst of session starts is all counted
    /// before the first prompt confirms.
    #[serde(default)]
    last_start_unix: i64,
    /// The same moment on the steady clock, with the boot it counts on
    /// ([`crate::linux_trial::Steady`]), so the next start, in another process, reads the
    /// age on a clock no NTP step moves ([`crate::linux_trial::judged_now`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_start_steady: Option<Steady>,
    /// A window start of this trial confirmed it. Until one has, window starts go on
    /// counting, and roll the install back past the budget, even once a terminal session
    /// confirmed the trial: a session's prompt proves the session lane only
    /// ([`crate::linux_trial::Lanes`]).
    #[serde(default)]
    window_proven: bool,
    /// The trial a prepared install displaced, put back if the install is abandoned before
    /// its rename ([`recover`]); dropped once the rename lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    displaced: Option<Box<Trial>>,
}

impl Trial {
    /// The record's lane flags, for the rule that decides them
    /// ([`crate::linux_trial::Lanes`]).
    fn lanes(&self) -> Lanes {
        Lanes {
            healthy: self.healthy,
            window_proven: self.window_proven,
        }
    }

    fn set_lanes(&mut self, lanes: Lanes) {
        self.healthy = lanes.healthy;
        self.window_proven = lanes.window_proven;
    }

    /// When the latest counted start began, on both clocks the record keeps.
    fn start_clock(&self) -> StartClock {
        StartClock {
            unix: self.last_start_unix,
            steady: self.last_start_steady.clone(),
        }
    }

    /// Stamp the latest counted start: both clocks together, never one alone.
    fn set_start_clock(&mut self, clock: StartClock) {
        self.last_start_unix = clock.unix;
        self.last_start_steady = clock.steady;
    }
}

/// What one start's confirmation found ([`confirm_lane_locked`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirmed {
    /// This start confirmed the trial.
    Yes,
    /// Nothing was waiting on it: no trial, one of another build, or one already
    /// confirmed.
    NotPending,
    /// This process runs some other file than the one on trial.
    OtherFile,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct State {
    schema: u32,
    target: PathBuf,
    enabled: bool,
    high_water: u64,
    min_build: u64,
    roster_floor: u64,
    revoked_machines: Vec<String>,
    /// Who raised [`Self::min_build`], so a revocation takes back what its machine raised.
    #[serde(default, skip_serializing_if = "FloorSources::is_empty")]
    min_build_sources: FloorSources,
    installed: Identity,
    trial: Option<Trial>,
    #[serde(default)]
    staged: Option<Identity>,
    rejected_build: u64,
    /// Whether [`Self::rejected_build`] was rolled back after its starts spent the trial's
    /// budget ([`rolls_back_unstarted`]), and not by `aterm update rollback` of a build
    /// that ran.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    rejected_unstarted: bool,
    /// The build whose trial ended at a crash loop no rollback could answer
    /// ([`rollback_refusal`], at the start past the budget in [`count_start_locked`]): it
    /// stays installed though no start of it finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kept_unproven: Option<u64>,
    /// What each crash loop's end says — a rollback or a build kept — until a check loop
    /// with a window to tell takes them ([`take_unsaid_trial_ends`],
    /// U21).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unsaid_trial_ends: Vec<String>,
    outcome: String,
    updated_at: String,
    failing_checks: u32,
    failing_kind: String,
    check_owner: String,
    check_repo: String,
    check_build: u64,
    last_attempt_unix: i64,
    next_check_unix: i64,
}

#[derive(Clone)]
struct Proof {
    appcast: Vec<u8>,
    signature: Vec<u8>,
    roster: Vec<u8>,
    roster_signature: Vec<u8>,
    policy: Option<(Vec<u8>, Vec<u8>)>,
}

/// The trust anchors: always the committed ones in production (unit tests inject others
/// through private functions; no environment or enrollment file replaces them).
#[derive(Clone, Copy)]
struct Pins<'a> {
    masters: &'a [&'a str],
}

const PRODUCTION_PINS: Pins<'static> = Pins {
    masters: aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
};

fn uid() -> u32 {
    // SAFETY: geteuid has no preconditions or failure mode.
    unsafe { libc::geteuid() }
}

fn native() -> Result<LinuxTarget, String> {
    #[cfg(all(test, not(target_os = "linux")))]
    let native = Some(LinuxTarget::Aarch64);
    #[cfg(not(all(test, not(target_os = "linux"))))]
    let native = LinuxTarget::native();
    native.ok_or_else(|| "this Linux architecture has no update target".into())
}

fn context() -> Result<&'static Context, String> {
    CONTEXT
        .get_or_init(|| {
            let target = std::env::current_exe().map_err(|e| e.to_string())?;
            Context::at(target)
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Read this process's executable path into [`context`] now, if it is not read yet (it is read once per process).
pub fn capture_context() {
    let _ = context();
}

impl Context {
    fn at(target: PathBuf) -> Result<Self, String> {
        let context = Self::destination(target)?;
        checked_file(&context.target, BINARY_LIMIT, true)?;
        Ok(context)
    }

    fn destination(target: PathBuf) -> Result<Self, String> {
        if !target.is_absolute() || target.file_name().is_none_or(|name| name != "aterm") {
            return Err("Linux self-update requires an installed executable named aterm".into());
        }
        let parent = target
            .parent()
            .ok_or("executable has no parent directory")?;
        ancestry::refusal(parent)?;
        let md = fs::symlink_metadata(parent).map_err(|e| e.to_string())?;
        if md.uid() != uid() || md.mode() & 0o200 == 0 {
            return Err("the executable directory is not owned and writable by this user".into());
        }
        if target.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        }) {
            return Err("update target must have a canonical absolute spelling".into());
        }
        Ok(Self {
            dir: parent.join(".aterm-update"),
            target,
            candidate_hashed: std::sync::Arc::default(),
        })
    }

    /// The SHA-256 of the held candidate, read again only when the file is not the one last
    /// read ([`FileStamp`]); `None` for a candidate that is missing, unsafe, or unreadable.
    fn candidate_digest(&self) -> Option<String> {
        let file = checked_file(&self.dir.join("candidate"), BINARY_LIMIT, false).ok()?;
        let stamp = FileStamp::of(&file.metadata().ok()?);
        let mut hashed = self
            .candidate_hashed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((seen, digest)) = hashed.as_ref()
            && *seen == stamp
        {
            return Some(digest.clone());
        }
        let digest = hash_open_file(file).ok()?;
        *hashed = Some((stamp, digest.clone()));
        Some(digest)
    }

    fn prepare_dir(&self) -> Result<(), String> {
        if let Ok(md) = fs::symlink_metadata(&self.dir)
            && (!md.is_dir() || md.uid() != uid() || md.mode() & 0o077 != 0)
        {
            return Err(format!(
                "{} isn\u{2019}t a private directory of yours (mode 0700, not a symlink)",
                self.dir.display()
            ));
        }
        aterm_update_core::ensure_private_dir(&self.dir).map_err(|e| e.to_string())
    }

    fn lock(&self) -> Result<FileLock, String> {
        self.prepare_dir()?;
        let path = self.dir.join("lock");
        if path.try_exists().map_err(|e| e.to_string())? {
            checked_file(&path, RECORD_LIMIT, false)?;
        }
        FileLock::acquire_within(&path, Duration::from_millis(500)).map_err(|e| {
            if e.kind() == std::io::ErrorKind::TimedOut {
                LOCK_BUSY.to_string()
            } else {
                format!("can\u{2019}t lock {}: {e}", path.display())
            }
        })
    }

    /// The durable state record. Not named `read`: the lock-order census knows a
    /// lock by its method name, and a state read held across `install_locked`'s
    /// own read would look like a re-entrant `context` lock.
    fn read_state(&self) -> Result<Option<State>, String> {
        let path = self.dir.join("state.toml");
        if !path.try_exists().map_err(|e| e.to_string())? {
            return Ok(None);
        }
        self.prepare_dir()?;
        let text = String::from_utf8(read_small(&path)?).map_err(|e| e.to_string())?;
        let state: State =
            aterm_toml::from_str(&text).map_err(|e| format!("invalid Linux update state: {e}"))?;
        if state.schema != 1 {
            return Err(format!("{} was written by a newer aterm", path.display()));
        }
        if state.target != self.target {
            return Err(format!(
                "{} belongs to the copy at {}",
                path.display(),
                state.target.display()
            ));
        }
        Ok(Some(state))
    }

    fn save(&self, state: &State) -> Result<(), String> {
        let text = aterm_toml::to_string(state).map_err(|e| e.to_string())?;
        write_atomic(&self.dir.join("state.toml"), text.as_bytes())
    }

    fn backup(&self, identity: &Identity) -> PathBuf {
        self.dir.join(format!("rollback-{}", identity.sha256))
    }
}

fn checked_file(path: &Path, cap: u64, executable: bool) -> Result<File, String> {
    let file = open_no_follow(path)?;
    let md = file.metadata().map_err(|e| e.to_string())?;
    if !file_is_safe(&md, cap, executable) {
        return Err(format!("unsafe update file: {}", path.display()));
    }
    Ok(file)
}

fn open_no_follow(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))
}

/// [`checked_file`]'s safety rule, on metadata already read: a regular file of this
/// user's, writable by no one else, within `cap`, and — for one that will be run —
/// executable and neither setuid nor setgid.
fn file_is_safe(md: &fs::Metadata, cap: u64, executable: bool) -> bool {
    md.is_file()
        && md.uid() == uid()
        && md.mode() & 0o022 == 0
        && md.len() <= cap
        && !(executable && (md.mode() & 0o111 == 0 || md.mode() & 0o6000 != 0))
}

/// A kept rollback copy, judged exactly as the rollback will open it
/// (`checked_file(.., BINARY_LIMIT, true)`).
enum KeptCopy {
    /// No such name.
    Missing,
    /// Not a regular file.
    NotAFile,
    /// A file the rollback's own open refuses on sight: another user's, writable by group
    /// or others, too big, not executable, setuid/setgid, or one it may not read.
    Unsafe,
    /// A file the rollback may open, with this digest.
    Digest(String),
}

/// What is at `backup`. `Err` only for a read that did not finish and may on a later try (an
/// I/O error, an interrupted call, no descriptor free); every answer about the file itself is
/// a [`KeptCopy`].
fn kept_copy(backup: &Path) -> Result<KeptCopy, String> {
    match fs::symlink_metadata(backup) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(KeptCopy::Missing),
        Err(error) => return Err(format!("read {}: {error}", backup.display())),
        Ok(meta) if !meta.is_file() => return Ok(KeptCopy::NotAFile),
        Ok(meta) if !file_is_safe(&meta, BINARY_LIMIT, true) || meta.mode() & 0o400 == 0 => {
            return Ok(KeptCopy::Unsafe);
        }
        Ok(_) => {}
    }
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(backup)
    {
        Ok(file) => file,
        Err(error) => {
            return match error.raw_os_error() {
                Some(libc::EACCES | libc::EPERM) => Ok(KeptCopy::Unsafe),
                Some(libc::ELOOP) => Ok(KeptCopy::NotAFile),
                Some(libc::ENOENT) => Ok(KeptCopy::Missing),
                _ => Err(format!("open {}: {error}", backup.display())),
            };
        }
    };
    let md = file.metadata().map_err(|e| e.to_string())?;
    if !file_is_safe(&md, BINARY_LIMIT, true) || md.mode() & 0o400 == 0 {
        return Ok(KeptCopy::Unsafe);
    }
    hash_open_file(file).map(KeptCopy::Digest)
}

fn read_small(path: &Path) -> Result<Vec<u8>, String> {
    read_bounded(path, RECORD_LIMIT)
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    checked_file(path, limit, false)?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("update record exceeds its size bound".into());
    }
    Ok(bytes)
}

fn sync_dir(path: &Path) -> Result<(), String> {
    crate::durable::sync_dir(path, aterm_durable::Flush::Durable)
        .map_err(|e| format!("sync {}: {e}", path.display()))
}

/// [`sync_dir`], only ordered ([`aterm_durable::ordered_dir`]). On Linux, where this lane
/// runs, that is the same full `fsync(2)`, so each caller names only what it needs.
fn ordered_dir(path: &Path) -> Result<(), String> {
    aterm_durable::ordered_dir(path).map_err(|e| format!("sync {}: {e}", path.display()))
}

fn fresh_file(parent: &Path, stem: &str) -> Result<(PathBuf, File), String> {
    let nonce = aterm_uds::rand::hex_token::<16>().map_err(|e| e.to_string())?;
    let path = parent.join(format!("{stem}-{nonce}"));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|e| e.to_string())?;
    Ok((path, file))
}

/// A fresh private file in `parent`, written by `write` (handed the file and its path) and
/// closed; removed again when `write` fails, so a failure leaves nothing behind.
fn fresh_written(
    parent: &Path,
    stem: &str,
    write: impl FnOnce(&mut File, &Path) -> Result<(), String>,
) -> Result<PathBuf, String> {
    let (temp, mut file) = fresh_file(parent, stem)?;
    let written = write(&mut file, &temp);
    drop(file);
    match written {
        Ok(()) => Ok(temp),
        Err(error) => {
            let _ = fs::remove_file(&temp);
            Err(error)
        }
    }
}

/// The stems [`fresh_file`]'s callers name their files with. Each writer holds the
/// copy's update lock for as long as its file exists, so one found under the lock is
/// a dead writer's ([`sweep_stale_temps`]).
const TEMP_STEMS: [&str; 6] = [
    "record",
    "download",
    "restore",
    "probe-output",
    "probe-error",
    "bootstrap",
];

/// Whether `name` is one [`fresh_file`] makes: a [`TEMP_STEMS`] stem, a dash, and its
/// 32 lowercase hex digits of nonce.
fn is_temp_name(name: &str) -> bool {
    TEMP_STEMS.iter().any(|stem| {
        name.strip_prefix(stem)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|nonce| {
                nonce.len() == 32
                    && nonce
                        .bytes()
                        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            })
    })
}

/// Remove what a killed writer left in the update folder (a download cut off by a closed tab
/// leaves `download-<nonce>`, and the next check uses a fresh nonce).
fn sweep_stale_temps(context: &Context) {
    let Ok(entries) = fs::read_dir(&context.dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_str().is_some_and(is_temp_name)
            && let Err(error) = fs::remove_file(entry.path())
        {
            aterm_log::warn!(
                "aterm-update: could not remove the stale update file {}: {error}",
                entry.path().display()
            );
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("update record has no parent")?;
    let (temp, mut file) = fresh_file(parent, "record")?;
    let result = (|| {
        file.write_all(bytes).map_err(|e| e.to_string())?;
        crate::durable::sync_all(&file).map_err(|e| e.to_string())?;
        fs::rename(&temp, path).map_err(|e| e.to_string())?;
        sync_dir(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

fn hash_file(path: &Path) -> Result<String, String> {
    hash_open_file(checked_file(path, BINARY_LIMIT, false)?)
}

#[cfg(test)]
thread_local! {
    /// How many files this thread has hashed ([`hash_open_file`]): what a pass costs,
    /// for the tests that pin it.
    static HASHED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn hash_open_file(mut file: File) -> Result<String, String> {
    #[cfg(test)]
    HASHED.with(|hashed| hashed.set(hashed.get() + 1));
    let mut hash = aterm_digest::Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        size = size.saturating_add(count as u64);
        if size > BINARY_LIMIT {
            return Err("binary grew beyond its size bound".into());
        }
        hash.update(&buffer[..count]);
    }
    Ok(aterm_codec::hex::encode_lower(&hash.finalize()))
}

fn verify_binary(
    path: &Path,
    manifest: &Manifest,
    target: LinuxTarget,
) -> Result<Identity, String> {
    let artifact = manifest
        .linux_artifact(target)?
        .ok_or("signed release has no artifact for this Linux target")?;
    let mut file = checked_file(path, BINARY_LIMIT, false)?;
    if file.metadata().map_err(|e| e.to_string())?.len() != artifact.size {
        return Err("Linux artifact size differs from the signed manifest".into());
    }
    let mut header = [0u8; 64];
    file.read_exact(&mut header).map_err(|e| e.to_string())?;
    target.validate_elf_header(&header)?;
    if hash_file(path)? != artifact.sha256 {
        return Err("Linux artifact SHA-256 differs from the signed manifest".into());
    }
    let commit = manifest
        .commit
        .as_deref()
        .ok_or("signed Linux release has no source commit")?;
    if !aterm_codec::hex::is_hex(commit, 40) || manifest.build_number == 0 {
        return Err("signed Linux release has no canonical build/source identity".into());
    }
    Ok(Identity {
        build: manifest.build_number,
        sha256: artifact.sha256.into(),
        version: manifest.version.clone(),
        commit: commit.to_ascii_lowercase(),
        machine_id: manifest.machine_id.clone(),
    })
}

/// The signed digest is checked before this function is called. A short,
/// environment-independent identity command proves loader viability and the
/// exact compiled release identity without enrolling or starting a terminal.
fn probe_candidate(
    path: &Path,
    manifest: &Manifest,
    target: LinuxTarget,
    private_dir: &Path,
    timeout: Duration,
) -> Result<(), String> {
    use aterm_uds::spawnfd::Stdio;
    let (output_path, output) = fresh_file(private_dir, "probe-output")?;
    let (error_path, error) = fresh_file(private_dir, "probe-error")?;
    let result = (|| {
        let mut child = aterm_uds::spawnfd::command(path)
            .args(["update", "identity"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::from(output))
            .stderr(Stdio::from(error))
            .spawn()
            .map_err(|e| format!("authenticated Linux candidate cannot start: {e}"))?;
        let deadline = Instant::now() + timeout;
        let status = loop {
            let oversized = [&output_path, &error_path]
                .into_iter()
                .any(|path| fs::metadata(path).map_or(true, |md| md.len() > 8192));
            if oversized || Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(
                    "authenticated Linux candidate identity probe exceeded its time/output bound"
                        .into(),
                );
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("Linux identity probe failed: {error}"));
                }
            }
        };
        if !status.success() {
            let said = read_bounded(&error_path, 8192)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default();
            return Err(probe_exit_refusal(
                &manifest.version,
                path,
                &status.to_string(),
                &said,
            ));
        }
        let bytes = read_bounded(&output_path, 8192)?;
        let identity: aterm_update_core::linux::BinaryIdentity = aterm_json::from_slice(&bytes)
            .map_err(|e| format!("Linux identity probe returned invalid JSON: {e}"))?;
        identity.verify_against(manifest, target)
    })();
    let _ = fs::remove_file(output_path);
    let _ = fs::remove_file(error_path);
    result
}

/// Why a candidate whose identity probe exited unsuccessfully cannot install: the first
/// line it printed on stderr, said as "can't run on this system" when that is the dynamic
/// loader refusing it (a release linked against a newer glibc than this system has).
fn probe_exit_refusal(version: &str, path: &Path, status: &str, stderr: &str) -> String {
    let Some(said) = stderr.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return format!("aterm {version} failed when checked here ({status})");
    };
    let loader = said.contains("GLIBC_")
        || said.contains("GLIBCXX_")
        || said.contains("error while loading shared libraries")
        || said.contains("not found (required by");
    let program = path.to_string_lossy();
    let said = said.strip_prefix(&format!("{program}: ")).unwrap_or(said);
    let said = said
        .strip_suffix(&format!(" (required by {program})"))
        .unwrap_or(said);
    let said: String = said.chars().take(300).collect();
    if loader {
        format!("aterm {version} can\u{2019}t run on this system: {said}")
    } else {
        format!("aterm {version} failed when checked here ({status}): {said}")
    }
}

fn initial_state(context: &Context, installed: Identity) -> State {
    State {
        schema: 1,
        target: context.target.clone(),
        enabled: false,
        high_water: installed.build,
        min_build: 0,
        roster_floor: 0,
        revoked_machines: Vec::new(),
        min_build_sources: FloorSources::default(),
        installed,
        trial: None,
        staged: None,
        rejected_build: 0,
        rejected_unstarted: false,
        kept_unproven: None,
        unsaid_trial_ends: Vec::new(),
        outcome: String::new(),
        updated_at: String::new(),
        failing_checks: 0,
        failing_kind: String::new(),
        check_owner: String::new(),
        check_repo: String::new(),
        check_build: 0,
        last_attempt_unix: 0,
        next_check_unix: 0,
    }
}

/// Bootstrap and reinstallation use the very same destination lock, admission floors, exact rollback inode and durable replacement journal as self-update.
pub fn install_release(
    target: &Path,
    candidate: &Path,
    proof_dir: &Path,
) -> Result<String, String> {
    let context = Context::destination(target.to_path_buf())?;
    let _lock = context.lock()?;
    let proof = Proof::read(proof_dir)?;
    install_locked(
        &context,
        candidate,
        &proof,
        PRODUCTION_PINS,
        |path, manifest, target| {
            probe_candidate(
                path,
                manifest,
                target,
                &context.dir,
                Duration::from_secs(10),
            )
        },
    )
}

fn install_locked(
    context: &Context,
    source: &Path,
    proof: &Proof,
    pins: Pins<'_>,
    mut probe: impl FnMut(&Path, &Manifest, LinuxTarget) -> Result<(), String>,
) -> Result<String, String> {
    let exists = match fs::symlink_metadata(&context.target) {
        Ok(_) => {
            checked_file(&context.target, BINARY_LIMIT, true)?;
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.to_string()),
    };
    let mut state = context.read_state()?.unwrap_or(initial_state(
        context,
        Identity {
            build: 0,
            version: "un-enrolled local executable".into(),
            commit: String::new(),
            sha256: if exists {
                hash_file(&context.target)?
            } else {
                String::new()
            },
            machine_id: None,
        },
    ));
    if exists {
        recover(context, &mut state)?;
    }
    if state.trial.as_ref().is_some_and(|trial| !trial.healthy) {
        return Err("bootstrap refuses an unresolved Linux update trial".into());
    }
    if !exists && (state.enabled || state.trial.is_some()) {
        return Err("the enrolled executable disappeared; refusing bootstrap replacement".into());
    }
    let manifest = authorize(context, &mut state, proof, pins)?;
    let identity = verify_binary(source, &manifest, native()?)?;
    if identity.build < state.high_water.max(state.min_build)
        || identity.build <= state.rejected_build
    {
        return Err("bootstrap candidate is below the durable build/failure floor".into());
    }
    let (temp, mut file) = fresh_file(&context.dir, "bootstrap")?;
    let copied = (|| {
        let input = checked_file(source, BINARY_LIMIT, true)?;
        let copied = std::io::copy(&mut input.take(BINARY_LIMIT + 1), &mut file)
            .map_err(|e| e.to_string())?;
        if copied > BINARY_LIMIT {
            return Err("bootstrap source grew beyond its bound".into());
        }
        file.set_permissions(fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        aterm_durable::ordered(&file).map_err(|e| e.to_string())?;
        verify_binary(&temp, &manifest, native()?).map(|_| ())
    })();
    drop(file);
    if let Err(error) = copied {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    let candidate = context.dir.join("candidate");
    fs::rename(&temp, &candidate).map_err(|e| e.to_string())?;
    ordered_dir(&context.dir)?;
    proof.save(&context.dir)?;
    if exists && state.installed.sha256 != hash_file(&context.target)? {
        return Err("bootstrap rollback baseline changed; refusing replacement".into());
    }
    if exists && identity.sha256 != state.installed.sha256 {
        apply_with_checkpoints(context, &mut state, proof, pins, &mut probe, |_| Ok(()))?;
    } else {
        probe(&candidate, &manifest, native()?)?;
        if hash_file(&candidate)? != identity.sha256 {
            return Err("bootstrap candidate changed during probe".into());
        }
        state.installed = identity;
        state.high_water = state.high_water.max(state.installed.build);
        context.save(&state)?;
        if !exists {
            fs::rename(&candidate, &context.target).map_err(|e| e.to_string())?;
            sync_dir(context.target.parent().ok_or("missing target parent")?)?;
        }
    }
    state.enabled = true;
    state.outcome = if state.trial.as_ref().is_some_and(|trial| !trial.healthy) {
        installed_pending(&state.installed.version)
    } else {
        "Updates are on for this copy".into()
    };
    context.save(&state)?;
    Ok(state.outcome)
}

impl Proof {
    fn read(dir: &Path) -> Result<Self, String> {
        let policy = match (
            dir.join(POLICY).try_exists(),
            dir.join(POLICY_SIG).try_exists(),
        ) {
            (Ok(false), Ok(false)) => None,
            (Ok(true), Ok(true)) => Some((
                read_bounded(&dir.join(POLICY), APPCAST_LIMIT)?,
                read_bounded(&dir.join(POLICY_SIG), 4096)?,
            )),
            _ => return Err("latest-policy proof must contain both appcast and signature".into()),
        };
        Ok(Self {
            policy,
            appcast: read_bounded(&dir.join(APPCAST), APPCAST_LIMIT)?,
            signature: read_bounded(&dir.join(APPCAST_SIG), 4096)?,
            roster: if PRODUCTION_PINS.masters.is_empty() {
                Vec::new()
            } else {
                read_bounded(&dir.join(ROSTER), 65536)?
            },
            roster_signature: if PRODUCTION_PINS.masters.is_empty() {
                Vec::new()
            } else {
                read_bounded(&dir.join(ROSTER_SIG), 4096)?
            },
        })
    }

    fn save(&self, dir: &Path) -> Result<(), String> {
        for (name, bytes) in [
            (APPCAST, &self.appcast),
            (APPCAST_SIG, &self.signature),
            (ROSTER, &self.roster),
            (ROSTER_SIG, &self.roster_signature),
        ] {
            write_atomic(&dir.join(name), bytes)?;
        }
        if let Some((appcast, signature)) = &self.policy {
            write_atomic(&dir.join(POLICY), appcast)?;
            write_atomic(&dir.join(POLICY_SIG), signature)?;
        } else {
            for name in [POLICY, POLICY_SIG] {
                match fs::remove_file(dir.join(name)) {
                    Ok(()) => (),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                    Err(error) => return Err(error.to_string()),
                }
            }
            ordered_dir(dir)?;
        }
        Ok(())
    }
}

fn now() -> Result<i64, String> {
    aterm_time::unix_secs()
        .ok_or_else(|| "system clock predates the epoch".to_string())
        .and_then(|secs| i64::try_from(secs).map_err(|e| e.to_string()))
}

/// This moment on both clocks a counted start is stamped with ([`StartClock`]): the
/// wall clock, and the steady clock where this system has one to read
/// ([`steady_clock`]).
fn start_clock() -> Result<StartClock, String> {
    Ok(StartClock {
        unix: now()?,
        steady: steady_clock(Path::new(BOOT_ID), Path::new(TIME_NAMESPACE)),
    })
}

/// The kernel's name for this boot: a random UUID drawn at every boot, the same in
/// every process of it.
const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";

/// This process's time namespace (`time:[<inode>]`), which may offset its monotonic
/// clock from the host's.
const TIME_NAMESPACE: &str = "/proc/self/ns/time";

/// Whole seconds on `CLOCK_MONOTONIC`, named by the boot (`boot_id`) and the time
/// namespace (`time_namespace`) they count on ([`Steady`]); `None` when the boot has no
/// name to read or the clock cannot be read, and the wall clock judges alone.
fn steady_clock(boot_id: &Path, time_namespace: &Path) -> Option<Steady> {
    let boot = fs::read_to_string(boot_id).ok()?;
    astream_sys::parse_boot_id(boot.as_bytes())?;
    let boot = boot.trim();
    let namespace = fs::read_link(time_namespace)
        .map(|link| link.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid, writable `timespec`; CLOCK_MONOTONIC always exists.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut now) } != 0 {
        return None;
    }
    Some(Steady {
        clock: format!("{boot} {namespace}"),
        secs: now.tv_sec,
    })
}

fn authorize(
    context: &Context,
    state: &mut State,
    proof: &Proof,
    pins: Pins<'_>,
) -> Result<Manifest, String> {
    let head = if let Some((appcast, signature)) = &proof.policy {
        let policy = Proof {
            appcast: appcast.clone(),
            signature: signature.clone(),
            policy: None,
            ..proof.clone()
        };
        Some(authorize_one(context, state, &policy, pins)?)
    } else {
        None
    };
    let manifest = authorize_one(context, state, proof, pins)?;
    if head.is_some_and(|head| head.build_number < manifest.build_number) {
        return Err("latest-policy appcast is older than the selected release".into());
    }
    Ok(manifest)
}

fn authorize_one(
    context: &Context,
    state: &mut State,
    proof: &Proof,
    pins: Pins<'_>,
) -> Result<Manifest, String> {
    if pins.masters.is_empty() {
        return Err("this build pins no paper master, so it verifies no update channel".into());
    }
    let verified = aterm_update_core::verify_roster(
        pins.masters,
        proof.roster.clone(),
        &proof.roster_signature,
    )
    .map_err(|e| format!("machine roster signature refused: {e:?}"))?;
    let roster = aterm_update_core::Roster::parse(&verified)
        .map_err(|e| format!("machine roster refused: {e:?}"))?;
    let unix = now()?;
    roster
        .admit(state.roster_floor, unix)
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
    context.save(state)?;
    let attribution = roster
        .authorize_appcast(&proof.appcast, &proof.signature, unix)
        .map_err(|e| format!("Linux appcast authorization refused: {e:?}"))?;
    let text = std::str::from_utf8(&proof.appcast).map_err(|e| e.to_string())?;
    let manifest = Manifest::parse(text)?;
    if state.revoked_machines.contains(&attribution.machine_id) {
        return Err("appcast signer is in the durable revocation set".into());
    }
    attribution
        .bind(manifest.machine_id.as_deref(), manifest.roster_seq)
        .map_err(|e| format!("Linux appcast signer attribution refused: {e:?}"))?;
    (state.min_build, state.min_build_sources) = state.min_build_sources.step(
        state.min_build,
        &state.revoked_machines,
        manifest.min_build.unwrap_or(0),
        Some(&attribution.machine_id),
    );
    context.save(state)?;
    Ok(manifest)
}

/// Explicit enrollment is separate from merely running a dev checkout.
pub fn enable(current_build: u64, proof_dir: Option<&Path>) -> Result<String, String> {
    native()?;
    let context = context()?;
    let _lock = context.lock()?;
    let baseline = Identity {
        build: current_build,
        sha256: hash_file(&context.target)?,
        version: aterm_types::version::app_version().into(),
        commit: String::new(),
        machine_id: None,
    };
    let mut state = context.read_state()?.unwrap_or(State {
        schema: 1,
        target: context.target.clone(),
        enabled: false,
        high_water: current_build,
        min_build: 0,
        roster_floor: 0,
        revoked_machines: Vec::new(),
        min_build_sources: FloorSources::default(),
        installed: baseline.clone(),
        trial: None,
        staged: None,
        rejected_build: 0,
        rejected_unstarted: false,
        kept_unproven: None,
        unsaid_trial_ends: Vec::new(),
        outcome: String::new(),
        updated_at: String::new(),
        failing_checks: 0,
        failing_kind: String::new(),
        check_owner: String::new(),
        check_repo: String::new(),
        check_build: 0,
        last_attempt_unix: 0,
        next_check_unix: 0,
    });
    if let Some(dir) = proof_dir {
        let proof = Proof::read(dir)?;
        let manifest = authorize(context, &mut state, &proof, PRODUCTION_PINS)?;
        let installed = verify_binary(&context.target, &manifest, native()?)?;
        probe_candidate(
            &context.target,
            &manifest,
            native()?,
            &context.dir,
            Duration::from_secs(10),
        )?;
        if installed.build != current_build
            || installed.build < state.high_water.max(state.min_build)
        {
            return Err(
                "installed release does not match its compiled build or durable update floor"
                    .into(),
            );
        }
        proof.save(&context.dir)?;
        state.installed = installed;
    } else if state.installed.sha256 != baseline.sha256 {
        return Err("installed bytes differ from the enrolled identity; a verified release proof is required".into());
    }
    state.enabled = true;
    state.high_water = state.high_water.max(current_build);
    state.outcome = if !crate::automatic() {
        "Updates are on for this copy \u{2014} automatic checks are off, so `aterm update check` \
         looks for a new release"
    } else if aterm_update_core::settings::update_auto_apply() {
        "Updates are on for this copy \u{2014} new releases install by themselves; running \
         sessions are never restarted"
    } else {
        "Updates are on for this copy \u{2014} new releases download by themselves and `aterm \
         update apply` installs them"
    }
    .into();
    context.save(&state)?;
    Ok(state.outcome)
}

fn recover(context: &Context, state: &mut State) -> Result<(), String> {
    let Some(trial) = state.trial.as_mut() else {
        return Ok(());
    };
    let digest = hash_file(&context.target)?;
    let still_installed = digest == state.installed.sha256;
    match trial.phase {
        Phase::Prepared if digest == trial.new.sha256 => {
            trial.phase = Phase::Installed;
            trial.displaced = None;
            state.installed = trial.new.clone();
            state.high_water = state.high_water.max(trial.new.build);
            state.staged = None;
            state.outcome = installed_pending(&trial.new.version);
            context.save(state)
        }
        Phase::Prepared if still_installed => {
            state.trial = trial.displaced.take().map(|displaced| *displaced);
            state.outcome = "recovered an interrupted pre-replacement transaction; the original executable remains installed".into();
            context.save(state)
        }
        Phase::RollbackPrepared if digest == trial.old.sha256 => {
            let trial = trial.clone();
            record_rollback(state, &trial);
            state.installed = trial.old;
            state.trial = None;
            context.save(state)
        }
        Phase::RollbackPrepared if digest == trial.new.sha256 => rollback_locked(context, state),
        Phase::Installed if digest == trial.new.sha256 => Ok(()),
        _ => Err("installed binary no longer matches either side of its update transaction".into()),
    }
}

fn apply_locked(
    context: &Context,
    state: &mut State,
    proof: &Proof,
    pins: Pins<'_>,
) -> Result<String, String> {
    apply_with_checkpoints(
        context,
        state,
        proof,
        pins,
        |path, manifest, target| {
            probe_candidate(
                path,
                manifest,
                target,
                &context.dir,
                Duration::from_secs(10),
            )
        },
        |_| Ok(()),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Checkpoint {
    Backup,
    Prepared,
    Replaced,
    Committed,
}

fn apply_with_checkpoints(
    context: &Context,
    state: &mut State,
    proof: &Proof,
    pins: Pins<'_>,
    mut probe: impl FnMut(&Path, &Manifest, LinuxTarget) -> Result<(), String>,
    mut checkpoint: impl FnMut(Checkpoint) -> Result<(), String>,
) -> Result<String, String> {
    recover(context, state)?;
    sweep_stale_temps(context);
    let manifest = authorize(context, state, proof, pins)?;
    let candidate = context.dir.join("candidate");
    let identity = verify_binary(&candidate, &manifest, native()?)?;
    if identity.build <= state.high_water
        || identity.build < state.min_build
        || identity.build <= state.rejected_build
    {
        return Err("Linux candidate is not newer than the durable build/failure floor".into());
    }
    if let Some(pending) = awaiting_start(state) {
        return Err(pending);
    }
    if hash_file(&context.target)? != state.installed.sha256 {
        return Err("the installed rollback source changed; refusing to replace it".into());
    }
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())?;
    probe(&candidate, &manifest, native()?)?;
    if hash_file(&candidate)? != identity.sha256 {
        return Err("authenticated candidate changed during its startup probe".into());
    }
    let inherited = match state.trial.as_ref().filter(|trial| {
        trial.phase == Phase::Installed
            && crate::linux_trial::installed_window_unproven(trial.lanes(), trial.starts)
    }) {
        Some(trial) if rollback_refusal(context, state, trial)?.is_none() => {
            Some(trial.old.clone())
        }
        _ => None,
    };
    let inherits = inherited.is_some();
    let old = inherited.unwrap_or_else(|| state.installed.clone());
    let backup = context.backup(&old);
    if !inherits {
        let stale = match kept_copy(&backup)? {
            KeptCopy::Missing => None,
            KeptCopy::NotAFile | KeptCopy::Unsafe => Some(true),
            KeptCopy::Digest(digest) => Some(digest != old.sha256),
        };
        if stale == Some(true) {
            fs::remove_file(&backup).map_err(|e| format!("remove stale rollback copy: {e}"))?;
        }
        if stale != Some(false) {
            fs::hard_link(&context.target, &backup)
                .map_err(|e| format!("preserve rollback inode: {e}"))?;
        }
    }
    if hash_file(&backup)? != old.sha256 {
        return Err("rollback copy does not match the installed identity".into());
    }
    aterm_durable::ordered(&checked_file(&backup, BINARY_LIMIT, true)?)
        .map_err(|e| e.to_string())?;
    ordered_dir(&context.dir)?;
    checkpoint(Checkpoint::Backup)?;
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())?;
    aterm_durable::ordered(&checked_file(&candidate, BINARY_LIMIT, true)?)
        .map_err(|e| e.to_string())?;
    let displaced = state.trial.take().map(|mut trial| {
        trial.displaced = None;
        Box::new(trial)
    });
    state.trial = Some(Trial {
        old,
        new: identity.clone(),
        phase: Phase::Prepared,
        starts: 0,
        healthy: false,
        last_start_unix: 0,
        last_start_steady: None,
        window_proven: false,
        displaced,
    });
    context.save(state)?;
    checkpoint(Checkpoint::Prepared)?;
    fs::rename(&candidate, &context.target)
        .map_err(|e| format!("replace Linux executable: {e}"))?;
    ordered_dir(
        context
            .target
            .parent()
            .ok_or("missing executable directory")?,
    )?;
    checkpoint(Checkpoint::Replaced)?;
    let trial = state.trial.as_mut().ok_or("missing prepared transaction")?;
    trial.phase = Phase::Installed;
    trial.displaced = None;
    state.installed = identity;
    state.staged = None;
    state.high_water = state.high_water.max(state.installed.build);
    state.outcome = installed_pending(&state.installed.version);
    state.failing_checks = 0;
    state.failing_kind.clear();
    context.save(state)?;
    prune_backups(context, state);
    checkpoint(Checkpoint::Committed)?;
    Ok(state.outcome.clone())
}

fn rollback_locked(context: &Context, state: &mut State) -> Result<(), String> {
    rollback_with_checkpoint(context, state, |_| Ok(()))
}

fn rollback_with_checkpoint(
    context: &Context,
    state: &mut State,
    mut after_replace: impl FnMut(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let trial = state
        .trial
        .clone()
        .ok_or("no Linux update is available to roll back")?;
    if let Some(refusal) = rollback_refusal(context, state, &trial)? {
        return Err(format!(
            "{} can\u{2019}t come back: {refusal}",
            restored_name(&trial.old)
        ));
    }
    if hash_file(&context.target)? != trial.new.sha256
        || hash_file(&context.backup(&trial.old))? != trial.old.sha256
    {
        return Err("rollback current/previous identities could not be proved".into());
    }
    sweep_stale_temps(context);
    let temp = fresh_written(&context.dir, "restore", |file, temp| {
        let mut previous = checked_file(&context.backup(&trial.old), BINARY_LIMIT, true)?;
        std::io::copy(&mut previous, file).map_err(|e| e.to_string())?;
        file.set_permissions(fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        aterm_durable::ordered(file).map_err(|e| e.to_string())?;
        if hash_file(temp)? != trial.old.sha256 {
            return Err("rollback copy changed".into());
        }
        Ok(())
    })?;
    state
        .trial
        .as_mut()
        .ok_or("missing rollback transaction")?
        .phase = Phase::RollbackPrepared;
    context.save(state)?;
    fs::rename(&temp, &context.target).map_err(|e| e.to_string())?;
    after_replace(&context.target)?;
    ordered_dir(
        context
            .target
            .parent()
            .ok_or("missing executable directory")?,
    )?;
    state.outcome = format!(
        "{} is back; aterm {} won\u{2019}t install again; existing sessions were not restarted",
        restored_name(&trial.old),
        trial.new.version
    );
    record_rollback(state, &trial);
    state.installed = trial.old;
    state.trial = None;
    context.save(state)?;
    prune_backups(context, state);
    Ok(())
}

/// The record of `trial` rolled back: the build refused from now on, whether it was a
/// crash loop's rollback ([`rolls_back_unstarted`]), and, for one, the words the first
/// check loop with a window to tell says ([`State::unsaid_trial_ends`]).
fn record_rollback(state: &mut State, trial: &Trial) {
    state.rejected_build = state.rejected_build.max(trial.new.build);
    state.rejected_unstarted = rolls_back_unstarted(trial);
    if state.rejected_unstarted {
        keep_unsaid_trial_end(
            state,
            crate::linux_notice::rollback_body(&trial.new.version, &restored_name(&trial.old)),
        );
    }
}

/// How many crash loops' ends the record keeps unsaid ([`State::unsaid_trial_ends`]): a
/// release a copy cannot start ends one trial, so more than this many unsaid means no
/// window has opened on the copy across several releases, and the oldest go.
const UNSAID_TRIAL_ENDS_KEPT: usize = 4;

/// Keep `words`, one crash loop's end, on the record until a window is told
/// ([`State::unsaid_trial_ends`]), after any end still unsaid.
fn keep_unsaid_trial_end(state: &mut State, words: String) {
    state.unsaid_trial_ends.push(words);
    let over = state
        .unsaid_trial_ends
        .len()
        .saturating_sub(UNSAID_TRIAL_ENDS_KEPT);
    state.unsaid_trial_ends.drain(..over);
}

/// Whether rolling `trial` back ends a crash loop: its starts spent the budget without
/// proving it ([`crate::linux_trial::start_rolls_back`]'s count, the only one the start
/// lane rolls back at, [`count_start_locked`]).
fn rolls_back_unstarted(trial: &Trial) -> bool {
    trial.starts > MAX_TRIAL_STARTS
}

/// Why `trial` can never be rolled back, or `None` when nothing rules it out: the build it
/// would restore is below the signed minimum-build floor (a yank), or its kept copy is gone
/// or no longer that build.
fn rollback_refusal(
    context: &Context,
    state: &State,
    trial: &Trial,
) -> Result<Option<&'static str>, String> {
    Ok(if trial.old.build < state.min_build {
        Some("it is older than the oldest build a signed release allows")
    } else {
        match kept_copy(&context.backup(&trial.old))? {
            KeptCopy::Missing => Some("the copy kept of it is gone"),
            KeptCopy::NotAFile => Some("the copy kept of it is not that build"),
            KeptCopy::Digest(digest) if digest != trial.old.sha256 => {
                Some("the copy kept of it is not that build")
            }
            KeptCopy::Unsafe => Some("the copy kept of it is not safe to restore"),
            KeptCopy::Digest(_) => None,
        }
    })
}

/// Remove every kept rollback copy but the one the trial on record can restore
/// (`rollback-<its old build's digest>`).
fn prune_backups(context: &Context, state: &State) {
    let keep = state.trial.as_ref().map(|trial| context.backup(&trial.old));
    let Ok(entries) = fs::read_dir(&context.dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let kept_copy = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("rollback-"));
        if kept_copy
            && keep.as_ref() != Some(&path)
            && let Err(error) = fs::remove_file(&path)
        {
            aterm_log::warn!(
                "aterm-update: could not remove the rollback copy {}: {error}",
                path.display()
            );
        }
    }
}

/// The executable a rollback restores, as a person names it: build 0 is the
/// un-enrolled local baseline `install_locked` records, which has no version.
fn restored_name(old: &Identity) -> String {
    if old.build == 0 {
        "the previous copy".into()
    } else {
        format!("aterm {}", old.version)
    }
}

pub fn rollback() -> Result<String, String> {
    let context = context()?;
    let _lock = context.lock()?;
    let mut state = context
        .read_state()?
        .ok_or("this executable is not enrolled for Linux updates")?;
    recover(context, &mut state)?;
    rollback_locked(context, &mut state)?;
    Ok(state.outcome)
}

pub fn apply() -> Result<String, String> {
    let context = context()?;
    let _lock = context.lock()?;
    let mut state = context.read_state()?.ok_or(ENABLE_REMEDY)?;
    if !state.enabled {
        return Err(ENABLE_REMEDY.into());
    }
    recover(context, &mut state)?;
    if let Some(pending) = awaiting_start(&state) {
        return Err(pending);
    }
    if !context
        .dir
        .join("candidate")
        .try_exists()
        .map_err(|e| e.to_string())?
    {
        return Err(
            "Nothing is downloaded to install \u{2014} `aterm update check` looks for a newer \
             release"
                .into(),
        );
    }
    let proof = Proof::read(&context.dir)?;
    apply_locked(context, &mut state, &proof, PRODUCTION_PINS)
}

/// A window of this executable is starting (the GUI entry's boot lane, or, for a launch that could not vouch for its display before opening it, the moment its event loop opened it: `crate::linux_window_opened`).
pub fn boot(current_build: u64, current_commit: &str) -> crate::ApplyOutcome {
    let result = context().and_then(|context| {
        boot_in(
            context,
            current_build,
            current_commit,
            proc_self_inode,
            start_clock,
        )
    });
    match result {
        Ok(_) => crate::ApplyOutcome::NoUpdate,
        Err(error) => crate::ApplyOutcome::Deferred(error),
    }
}

/// [`boot`] against `context`, so a test drives the window lane on a fixture copy.
fn boot_in(
    context: &Context,
    current_build: u64,
    current_commit: &str,
    actual_inode: impl FnOnce() -> Result<(u64, u64), String>,
    now: impl FnOnce() -> Result<StartClock, String>,
) -> Result<bool, String> {
    let owed = context
        .read_state()?
        .and_then(|state| state.trial)
        .is_some_and(|trial| {
            trial.phase != Phase::Installed
                || crate::linux_trial::start_counts(Lane::Window, trial.lanes())
        });
    if !owed {
        return Ok(false);
    }
    let _lock = context.lock()?;
    let mut state = context.read_state()?.ok_or(MISSING_ENROLLMENT)?;
    let actual_inode = actual_inode()?;
    boot_locked(
        context,
        &mut state,
        current_build,
        current_commit,
        actual_inode,
        now()?,
    )
}

/// An interactive terminal session of this executable is starting (`aterm`'s session lane).
pub fn session_started(current_build: u64, current_commit: &str) -> bool {
    SESSION_LANE.store(true, Ordering::Release);
    let Ok(context) = context() else {
        return false;
    };
    match session_started_stamping_in(
        context,
        current_build,
        current_commit,
        proc_self_inode,
        start_clock,
    ) {
        Ok((starts_trial, stamped)) => {
            if let Some(stamped) = stamped {
                *SESSION_STAMP
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(stamped);
            }
            starts_trial
        }
        Err(error) => {
            aterm_log::warn!(
                "aterm-update: this session's start of the installed update did not finish: \
                 {error}"
            );
            false
        }
    }
}

/// The stamp this process's start left on the trial, when it was one of the budget's
/// starts ([`session_started`]): what [`defer_session_verdict`] moves forward, once,
/// while it is still the trial's.
static SESSION_STAMP: std::sync::Mutex<Option<StartClock>> = std::sync::Mutex::new(None);

/// A terminal session of this executable that has been up for [`crate::LINUX_SESSION_SILENT_DEFER`] with nothing from its shell yet.
pub fn defer_session_verdict(
    current_build: u64,
    current_commit: &str,
    pause: impl FnMut(Duration) -> bool,
) -> bool {
    let Some(own) = SESSION_STAMP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    else {
        return false;
    };
    let Ok(context) = context() else {
        return false;
    };
    defer_session_verdict_retrying(
        &own,
        || {
            defer_session_verdict_in(
                context,
                current_build,
                current_commit,
                proc_self_inode,
                &own,
                start_clock,
            )
        },
        start_clock,
        pause,
    )
}

/// Whether a deferral of the stamp `own` may still move it at `now`: later, and while
/// `own` is young on its own clock ([`crate::linux_trial::defer_moves`],
/// [`crate::linux_trial::judged_now`]).
fn defer_still_moves(own: &StartClock, now: &StartClock) -> bool {
    crate::linux_trial::defer_moves(own.unix, crate::linux_trial::judged_now(own, now))
}

/// How long a silent session's deferral that did not land waits before it tries again
/// ([`defer_session_verdict`]): the confirmer's own retry pace.
const SESSION_DEFER_RETRY: Duration = Duration::from_secs(1);

/// [`defer_session_verdict`]'s tries of moving the stamp `own`: `attempt` until it answers
/// (`Ok`: moved or refused, either final), retrying a failure after
/// `pause(SESSION_DEFER_RETRY)` while `own` may still move ([`defer_still_moves`]) and
/// `pause` says go on (`false` once the shell has spoken).
fn defer_session_verdict_retrying(
    own: &StartClock,
    mut attempt: impl FnMut() -> Result<bool, String>,
    mut now: impl FnMut() -> Result<StartClock, String>,
    mut pause: impl FnMut(Duration) -> bool,
) -> bool {
    loop {
        let error = match attempt() {
            Ok(moved) => return moved,
            Err(error) => error,
        };
        if !now().is_ok_and(|now| defer_still_moves(own, &now)) {
            aterm_log::info!(
                "aterm-update: this silent session's deferral did not land before the \
                 trial's verdict came due: {error}"
            );
            return false;
        }
        aterm_log::debug!(
            "aterm-update: this silent session's deferral did not land yet ({error}); it \
             tries again"
        );
        if !pause(SESSION_DEFER_RETRY) {
            return false;
        }
    }
}

/// [`defer_session_verdict`] against `context`, for the tests: under the lock, move the
/// pending trial's stamp to `now` when it is still `own` (the same build and commit, this
/// process running the file on trial, a trial the session lane still owes, its stamp not
/// moved since), only later than `own` and while `own` is still young
/// ([`crate::linux_trial::defer_moves`]): a settled stamp is a verdict due, which a late
/// deferral must not take back.
fn defer_session_verdict_in(
    context: &Context,
    current_build: u64,
    current_commit: &str,
    actual_inode: impl FnOnce() -> Result<(u64, u64), String>,
    own: &StartClock,
    now: impl FnOnce() -> Result<StartClock, String>,
) -> Result<bool, String> {
    let pending = context
        .read_state()?
        .and_then(|state| state.trial)
        .is_some_and(|trial| !trial.healthy && trial.start_clock() == *own);
    if !pending {
        return Ok(false);
    }
    let _lock = context.lock()?;
    let mut state = context.read_state()?.ok_or(MISSING_ENROLLMENT)?;
    let Some(trial) = state.trial.as_mut() else {
        return Ok(false);
    };
    if trial.phase != Phase::Installed
        || !crate::linux_trial::start_counts(Lane::Session, trial.lanes())
        || trial.new.build != current_build
        || !crate::commit_matches(&trial.new.commit, current_commit)
        || trial.start_clock() != *own
    {
        return Ok(false);
    }
    let installed = checked_file(&context.target, BINARY_LIMIT, true)?
        .metadata()
        .map_err(|e| e.to_string())?;
    if actual_inode()? != (installed.dev(), installed.ino()) {
        return Ok(false);
    }
    let now = now()?;
    if !defer_still_moves(own, &now) {
        return Ok(false);
    }
    trial.set_start_clock(now);
    context.save(&state)?;
    Ok(true)
}

/// This process started in the session lane ([`session_started`]): a terminal session,
/// or a `--headless` instance, whose [`confirm`] confirms that lane and never the window's.
static SESSION_LANE: AtomicBool = AtomicBool::new(false);

/// The lane this process's [`confirm`] confirms: the one its start was counted in.
fn confirm_lane(session_started: bool) -> Lane {
    if session_started {
        Lane::Session
    } else {
        Lane::Window
    }
}

/// This process's own executable inode: the file it runs, not whichever one a sibling
/// update most recently placed at the install path.
fn proc_self_inode() -> Result<(u64, u64), String> {
    let actual = fs::metadata("/proc/self/exe").map_err(|e| e.to_string())?;
    Ok((actual.dev(), actual.ino()))
}

/// [`session_started_stamping_in`]'s answer whether this start is one of the trial's,
/// for the tests that ask nothing more.
#[cfg(test)]
fn session_started_in(
    context: &Context,
    current_build: u64,
    current_commit: &str,
    actual_inode: impl FnOnce() -> Result<(u64, u64), String>,
    now: impl FnOnce() -> Result<StartClock, String>,
) -> Result<bool, String> {
    session_started_stamping_in(context, current_build, current_commit, actual_inode, now)
        .map(|(starts_trial, _)| starts_trial)
}

/// [`session_started`] against `context`, so a test drives the session lane on a fixture
/// copy: whether this start is one of the trial's, and the stamp it left when it was one of
/// the budget's starts ([`crate::linux_trial::start_stamp`]), which
/// [`defer_session_verdict_in`] may move forward.
fn session_started_stamping_in(
    context: &Context,
    current_build: u64,
    current_commit: &str,
    actual_inode: impl FnOnce() -> Result<(u64, u64), String>,
    now: impl FnOnce() -> Result<StartClock, String>,
) -> Result<(bool, Option<StartClock>), String> {
    let pending = context
        .read_state()?
        .and_then(|state| state.trial)
        .is_some_and(|trial| !trial.healthy);
    if !pending {
        return Ok((false, None));
    }
    let _lock = context.lock()?;
    let mut state = context.read_state()?.ok_or(MISSING_ENROLLMENT)?;
    let actual_inode = actual_inode()?;
    let now = now()?;
    let starts_trial = count_start_locked(
        context,
        &mut state,
        Lane::Session,
        current_build,
        current_commit,
        actual_inode,
        now.clone(),
    )?;
    let stamped = state
        .trial
        .as_ref()
        .filter(|trial| {
            starts_trial
                && trial.starts <= crate::LINUX_TRIAL_LAUNCHES
                && trial.start_clock() == now
        })
        .map(Trial::start_clock);
    Ok((starts_trial, stamped))
}

/// A window start counted against a pending trial ([`count_start_locked`]).
fn boot_locked(
    context: &Context,
    state: &mut State,
    current_build: u64,
    current_commit: &str,
    actual_inode: (u64, u64),
    now: impl Into<StartClock>,
) -> Result<bool, String> {
    count_start_locked(
        context,
        state,
        Lane::Window,
        current_build,
        current_commit,
        actual_inode,
        now.into(),
    )
}

/// Count one start of this executable, from `lane`, against a pending trial, at `now` (read
/// holding the lock this count runs under: [`session_started_stamping_in`]); `true` when
/// this process is a start of that trial (and should confirm it).
fn count_start_locked(
    context: &Context,
    state: &mut State,
    lane: Lane,
    current_build: u64,
    current_commit: &str,
    actual_inode: (u64, u64),
    now: StartClock,
) -> Result<bool, String> {
    recover(context, state)?;
    let Some(trial) = state.trial.as_mut() else {
        return Ok(false);
    };
    if trial.phase != Phase::Installed || !crate::linux_trial::start_counts(lane, trial.lanes()) {
        return Ok(false);
    }
    let installed = checked_file(&context.target, BINARY_LIMIT, true)?
        .metadata()
        .map_err(|e| e.to_string())?;
    if actual_inode != (installed.dev(), installed.ino()) {
        return Ok(false);
    }
    if trial.new.build != current_build || !crate::commit_matches(&trial.new.commit, current_commit)
    {
        state.outcome =
            "installed trial reports a different compiled identity; this startup is unhealthy"
                .into();
    }
    let previous = trial.start_clock();
    let previous_start = previous.unix;
    let now_judged = crate::linux_trial::judged_now(&previous, &now);
    trial.starts = trial.starts.saturating_add(1);
    if crate::linux_trial::stamp_lost(trial.starts, previous_start) {
        trial.set_start_clock(now);
        context.save(state)?;
        return Ok(true);
    }
    trial.set_start_clock(crate::linux_trial::start_stamp(trial.starts, previous, now));
    context.save(state)?;
    let Some(trial) = state.trial.clone().filter(|trial| {
        crate::linux_trial::start_rolls_back(trial.starts, previous_start, now_judged)
    }) else {
        return Ok(true);
    };
    let (restored, new) = (restored_name(&trial.old), &trial.new.version);
    if let Some(refusal) = rollback_refusal(context, state, &trial)? {
        state.trial = None;
        state.kept_unproven = Some(trial.new.build);
        keep_unsaid_trial_end(state, crate::linux_notice::kept_body(new));
        state.outcome = format!(
            "aterm {new} didn\u{2019}t start properly in {MAX_TRIAL_STARTS} launches, but \
             {restored} can\u{2019}t come back ({refusal}), so aterm {new} stays until a newer \
             release installs over it"
        );
        context.save(state)?;
        prune_backups(context, state);
        return Ok(false);
    }
    rollback_locked(context, state)?;
    state.outcome = format!(
        "aterm {new} didn\u{2019}t start properly in {MAX_TRIAL_STARTS} launches, so \
         {restored} is back and aterm {new} won\u{2019}t install again"
    );
    context.save(state)?;
    Ok(false)
}

/// A start of this executable reached its healthy checkpoint in the GUI entry (a window's first frame, or a `--headless` instance's bound socket).
pub fn confirm(current_build: u64, current_commit: &str) -> bool {
    let lane = confirm_lane(SESSION_LANE.load(Ordering::Acquire));
    (|| -> Result<bool, String> {
        let Ok(context) = context() else {
            return Ok(true);
        };
        let owes = |state: &State| {
            state.trial.as_ref().is_some_and(|trial| {
                trial.phase != Phase::Installed
                    || crate::linux_trial::start_counts(lane, trial.lanes())
            })
        };
        if !context.read_state()?.is_some_and(|state| owes(&state)) {
            return Ok(true);
        }
        let _lock = context.lock()?;
        let mut state = context.read_state()?.ok_or(MISSING_ENROLLMENT)?;
        let executable = File::open("/proc/self/exe").map_err(|e| e.to_string())?;
        let actual = executable.metadata().map_err(|e| e.to_string())?;
        confirm_lane_locked(
            context,
            &mut state,
            lane,
            current_build,
            current_commit,
            (actual.dev(), actual.ino()),
        )
        .map(|confirmed| matches!(confirmed, Confirmed::Yes | Confirmed::NotPending))
    })()
    .unwrap_or(false)
}

/// An interactive terminal session of this executable proved healthy (its shell's first output reached the terminal).
pub fn confirm_session(current_build: u64, current_commit: &str) -> crate::LinuxSessionConfirm {
    let Ok(context) = context() else {
        return crate::LinuxSessionConfirm::Done;
    };
    match confirm_session_in(context, current_build, current_commit, proc_self_inode) {
        Ok(Confirmed::Yes | Confirmed::NotPending | Confirmed::OtherFile) => {
            crate::LinuxSessionConfirm::Done
        }
        Err(error) => {
            aterm_log::debug!("aterm-update: this session's confirmation did not land: {error}");
            crate::LinuxSessionConfirm::Failed
        }
    }
}

/// [`confirm_session`] against `context`, for the tests.
fn confirm_session_in(
    context: &Context,
    current_build: u64,
    current_commit: &str,
    actual_inode: impl FnOnce() -> Result<(u64, u64), String>,
) -> Result<Confirmed, String> {
    if context.read_state()?.is_none() {
        return Ok(Confirmed::NotPending);
    }
    let _lock = context.lock()?;
    let mut state = context.read_state()?.ok_or(MISSING_ENROLLMENT)?;
    confirm_lane_locked(
        context,
        &mut state,
        Lane::Session,
        current_build,
        current_commit,
        actual_inode()?,
    )
}

/// A window's confirmation ([`confirm_lane_locked`]): `false` only for a process
/// running some other file than the one on trial.
#[cfg(test)]
fn confirm_locked(
    context: &Context,
    state: &mut State,
    current_build: u64,
    current_commit: &str,
    actual_inode: (u64, u64),
) -> Result<bool, String> {
    confirm_lane_locked(
        context,
        state,
        Lane::Window,
        current_build,
        current_commit,
        actual_inode,
    )
    .map(|confirmed| matches!(confirmed, Confirmed::Yes | Confirmed::NotPending))
}

/// Confirm the pending trial from `lane`. Every lane needs the trial's own build and commit
/// and this process running the very file on trial.
fn confirm_lane_locked(
    context: &Context,
    state: &mut State,
    lane: Lane,
    current_build: u64,
    current_commit: &str,
    actual_inode: (u64, u64),
) -> Result<Confirmed, String> {
    recover(context, state)?;
    let Some(trial) = state.trial.as_mut() else {
        return Ok(Confirmed::NotPending);
    };
    if trial.phase != Phase::Installed
        || trial.new.build != current_build
        || !crate::commit_matches(&trial.new.commit, current_commit)
    {
        return Ok(Confirmed::NotPending);
    }
    let installed = checked_file(&context.target, BINARY_LIMIT, true)?
        .metadata()
        .map_err(|e| e.to_string())?;
    if actual_inode != (installed.dev(), installed.ino()) {
        return Ok(Confirmed::OtherFile);
    }
    let LaneConfirm::Confirms { lanes, settles } = crate::linux_trial::confirm(lane, trial.lanes())
    else {
        return Ok(Confirmed::NotPending);
    };
    trial.set_lanes(lanes);
    trial.starts = 0;
    trial.set_start_clock(StartClock::default());
    if settles {
        state.outcome = format!("aterm {} is installed", trial.new.version);
    }
    context.save(state)?;
    Ok(Confirmed::Yes)
}

fn download_proof(
    source: &Source,
    tag: &str,
    current_roster: Option<&Proof>,
) -> Result<Proof, String> {
    download_named_proof(source, tag, APPCAST, current_roster)
}

fn download_named_proof(
    source: &Source,
    tag: &str,
    appcast_name: &str,
    current_roster: Option<&Proof>,
) -> Result<Proof, String> {
    let fetch = |name: &str, limit| {
        let url =
            aterm_update_core::cdn::release_download_url(&source.owner, &source.repo, tag, name)
                .ok_or("unsafe release asset address")?;
        aterm_update_core::download_bytes(&url, limit)
    };
    let appcast = fetch(appcast_name, APPCAST_LIMIT).map_err(|error| {
        if aterm_update_core::download_error_is_not_found(&error) {
            format!("missing appcast: {error}")
        } else {
            error
        }
    })?;
    Ok(Proof {
        policy: current_roster.map(|head| (head.appcast.clone(), head.signature.clone())),
        appcast,
        signature: fetch(&format!("{appcast_name}.sig"), 4096)?,
        roster: if let Some(proof) = current_roster {
            proof.roster.clone()
        } else if PRODUCTION_PINS.masters.is_empty() {
            Vec::new()
        } else {
            fetch(ROSTER, 65536)?
        },
        roster_signature: if let Some(proof) = current_roster {
            proof.roster_signature.clone()
        } else if PRODUCTION_PINS.masters.is_empty() {
            Vec::new()
        } else {
            fetch(ROSTER_SIG, 4096)?
        },
    })
}

fn authenticate_tag(
    context: &Context,
    state: &mut State,
    tag: &str,
    proof: &Proof,
    pins: Pins<'_>,
) -> Result<Manifest, String> {
    let manifest = authorize(context, state, proof, pins)?;
    if !matches!(
        aterm_update_core::tag::parse_release_tag(tag),
        Ok(aterm_update_core::tag::TagKind::Candidate(_))
    ) || tag != format!("v{}", manifest.version)
    {
        return Err("signed appcast version does not match the elected release tag".into());
    }
    Ok(manifest)
}

/// The channel head, authenticated, and the held stage proved again under whatever roster
/// that authentication admitted, whatever verdict the head itself gets: `authorize_one`
/// saves the ratcheted `roster_floor` before it judges the head's appcast, signer,
/// attribution, policy ordering and tag, so a refused head has still moved the floor, and
/// the reroot ([`reroot_held_stage_proof`]) runs on both arms.
fn authenticate_head(
    context: &Context,
    state: &mut State,
    tag: &str,
    proof: &Proof,
    pins: Pins<'_>,
) -> Result<Manifest, String> {
    let authenticated = authenticate_tag(context, state, tag, proof, pins);
    let rerooted = reroot_held_stage_proof(context, state, proof, pins);
    let manifest = authenticated?;
    rerooted?;
    Ok(manifest)
}

fn discovered_head(
    pointer: Result<aterm_update_core::pointer::Pointer, aterm_update_core::pointer::PointerError>,
) -> Result<String, String> {
    match pointer {
        Ok(pointer) => Ok(pointer.tag),
        Err(aterm_update_core::pointer::PointerError::OtherTag { tag }) => Ok(tag),
        Err(error) => Err(error.to_string()),
    }
}

/// What one [`check_locked`] pass did, which decides what the record books for it
/// ([`check_recording`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Checked {
    /// The channel was read: the pass is the record's last check (`updated_at`), and
    /// it ends a failure streak.
    Channel,
    /// Nothing was asked of the channel: an installed update waits on its trial's start,
    /// and checks wait with it.
    Nothing,
}

fn check_locked(
    context: &Context,
    state: &mut State,
    source: &Source,
    provider: &crate::CheckSettingsProvider,
) -> Result<Checked, String> {
    recover(context, state)?;
    sweep_stale_temps(context);
    if state
        .trial
        .as_ref()
        .is_some_and(|trial| trial.phase == Phase::Installed && !trial.healthy)
    {
        // No check: the channel is not read while a trial is pending. A check holds
        if state.failing_checks == 0 {
            state.outcome = format!(
                "aterm {} is installed \u{2014} the next aterm you start finishes it; update \
                 checks wait until then",
                state.installed.version
            );
        }
        context.save(state)?;
        return Ok(Checked::Nothing);
    }
    let head = discovered_head(aterm_update_core::pointer::resolve(
        &source.owner,
        &source.repo,
        APPCAST,
        &|tag| {
            matches!(
                aterm_update_core::tag::parse_release_tag(tag),
                Ok(aterm_update_core::tag::TagKind::Candidate(_))
            )
        },
    ))?;
    let proof = match download_proof(source, &head, None) {
        Ok(proof) => proof,
        Err(error) if error.starts_with("missing appcast: ") => {
            state.outcome =
                format!("channel head {head} has no app manifest yet — nothing to install from it");
            return context.save(state).map(|()| Checked::Channel);
        }
        Err(error) => return Err(error),
    };
    let tag = head;
    let manifest = authenticate_head(context, state, &tag, &proof, PRODUCTION_PINS)?;
    let target = native()?;
    if manifest.linux_artifact(target)?.is_none() {
        state.outcome = format!(
            "channel head {tag} carries no {} executable — nothing to install from it",
            target.triple()
        );
        return context.save(state).map(|()| Checked::Channel);
    }
    let artifact = manifest
        .linux_artifact(target)?
        .ok_or("elected Linux artifact disappeared")?;
    if manifest.build_number < state.min_build {
        return Err("the published Linux build is below the signed minimum-build floor; no install permitted".into());
    }
    if manifest.build_number <= state.high_water || manifest.build_number <= state.rejected_build {
        state.outcome = if manifest.build_number <= state.installed.build {
            format!(
                "Up to date \u{2014} the newest release is aterm {}",
                manifest.version
            )
        } else if manifest.build_number == state.rejected_build {
            format!(
                "aterm {} was rolled back on this copy, so it won\u{2019}t install again",
                manifest.version
            )
        } else {
            format!(
                "aterm {} is older than a release this copy rolled back, so it won\u{2019}t install",
                manifest.version
            )
        };
        return context.save(state).map(|()| Checked::Channel);
    }
    let url = aterm_update_core::cdn::release_download_url(
        &source.owner,
        &source.repo,
        &tag,
        artifact.name,
    )
    .ok_or("unsafe Linux artifact name")?;
    if !reusable_stage(context, state, &manifest, target) {
        let (temp, file) = fresh_file(&context.dir, "download")?;
        drop(file);
        let downloaded = aterm_update_core::download_to(&url, &temp, artifact.size)
            .and_then(|()| verify_binary(&temp, &manifest, target).map(|_| ()));
        if let Err(error) = downloaded {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }
        clear_stage(context, state)?;
        fs::rename(&temp, context.dir.join("candidate")).map_err(|e| e.to_string())?;
        ordered_dir(&context.dir)?;
    }
    clear_stage(context, state)?;
    proof.save(&context.dir)?;
    finish_candidate(
        context,
        state,
        &proof,
        PRODUCTION_PINS,
        source,
        provider,
        |path, manifest, target| {
            probe_candidate(
                path,
                manifest,
                target,
                &context.dir,
                Duration::from_secs(10),
            )
        },
    )?;
    Ok(Checked::Channel)
}

/// A held stage is proved again under the roster this check admitted. `authorize_one`
/// ratchets `roster_floor` on every check, even of a head with nothing to install, while
/// the held stage keeps the proof it was staged with, whose older roster `apply` would
/// then refuse as a replay (`Rollback`).
fn reroot_held_stage_proof(
    context: &Context,
    state: &mut State,
    head: &Proof,
    pins: Pins<'_>,
) -> Result<(), String> {
    let Some(stage_build) = state.staged.as_ref().map(|stage| stage.build) else {
        return Ok(());
    };
    let Ok(saved) = Proof::read(&context.dir) else {
        return Ok(());
    };
    if saved.roster == head.roster && saved.roster_signature == head.roster_signature {
        return Ok(());
    }
    let rerooted = Proof {
        roster: head.roster.clone(),
        roster_signature: head.roster_signature.clone(),
        ..saved
    };
    match authorize(context, state, &rerooted, pins) {
        Ok(manifest) if manifest.build_number == stage_build => rerooted.save(&context.dir),
        Ok(_) | Err(_) => Ok(()),
    }
}

/// Whether the candidate on disk serves `manifest` without a download: its bytes are the
/// very artifact the authenticated manifest names, and a stage on record is one that may
/// still install and is that release.
fn reusable_stage(
    context: &Context,
    state: &State,
    manifest: &Manifest,
    target: LinuxTarget,
) -> bool {
    if state.staged.as_ref().is_some_and(|stage| {
        !stage_admissible(state, stage) || stage.build != manifest.build_number
    }) {
        return false;
    }
    verify_binary(&context.dir.join("candidate"), manifest, target).is_ok_and(|identity| {
        state
            .staged
            .as_ref()
            .is_none_or(|stage| identity.sha256 == stage.sha256)
    })
}

fn automatic_apply_allowed(source: &Source, provider: &crate::CheckSettingsProvider) -> bool {
    let Some(current) = provider() else {
        return false;
    };
    current.source == *source && current.auto_apply
}

/// Retire presentation authority before changing its bytes/proof. A failed
/// download admission or identity probe must never resurrect an older stage.
fn clear_stage(context: &Context, state: &mut State) -> Result<(), String> {
    if state.staged.take().is_some() {
        context.save(state)?;
    }
    Ok(())
}

/// Why [`automatic_apply_allowed`] holds a stage, read from the same provider. The
/// install switch is named as Settings paints it
/// ([`aterm_update_core::settings::install_switch_off`]), never by its key.
fn apply_held_reason(source: &Source, provider: &crate::CheckSettingsProvider) -> String {
    match provider() {
        None => "the update settings couldn\u{2019}t be confirmed".to_string(),
        Some(current) if current.source != *source => {
            "the update channel changed during the check".to_string()
        }
        Some(current) if !current.auto_apply => aterm_update_core::settings::install_switch_off(),
        Some(_) => "the update settings changed during the check".to_string(),
    }
}

fn held_stage(context: &Context, state: &mut State, why: &str) -> Result<String, String> {
    let stage = state
        .staged
        .as_ref()
        .ok_or("missing verified Linux stage")?;
    state.outcome = format!(
        "aterm {} is downloaded \u{2014} `aterm update apply` installs it ({why})",
        stage.version
    );
    context.save(state)?;
    Ok(state.outcome.clone())
}

/// A stage buys no replacement authority. Both the initial decision and the
/// final post-probe/pre-rename boundary sample the current source/apply policy.
fn finish_candidate(
    context: &Context,
    state: &mut State,
    proof: &Proof,
    pins: Pins<'_>,
    source: &Source,
    provider: &crate::CheckSettingsProvider,
    mut probe: impl FnMut(&Path, &Manifest, LinuxTarget) -> Result<(), String>,
) -> Result<String, String> {
    clear_stage(context, state)?;
    let manifest = authorize(context, state, proof, pins)?;
    let candidate = context.dir.join("candidate");
    let identity = verify_binary(&candidate, &manifest, native()?)?;
    if identity.build <= state.high_water
        || identity.build < state.min_build
        || identity.build <= state.rejected_build
    {
        return Err("Linux stage is below its durable build/failure floor".into());
    }
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())?;
    probe(&candidate, &manifest, native()?)?;
    if hash_file(&candidate)? != identity.sha256 {
        return Err("Linux stage changed during its identity probe".into());
    }
    state.staged = Some(identity);
    context.save(state)?;
    if !automatic_apply_allowed(source, provider) {
        return held_stage(context, state, &apply_held_reason(source, provider));
    }
    let mut held = false;
    let result = apply_with_checkpoints(context, state, proof, pins, &mut probe, |at| {
        if at == Checkpoint::Prepared && !automatic_apply_allowed(source, provider) {
            held = true;
            Err("automatic Linux replacement vetoed before rename".into())
        } else {
            Ok(())
        }
    });
    if held {
        recover(context, state)?;
        return held_stage(context, state, &apply_held_reason(source, provider));
    }
    result
}

fn automatic_check_due(state: &State, source: &Source, started: i64) -> bool {
    state.check_owner != source.owner
        || state.check_repo != source.repo
        || started >= state.next_check_unix
}

fn request_apply_provider(
    provider: &crate::CheckSettingsProvider,
    requested_auto_apply: bool,
) -> crate::CheckSettingsProvider {
    let current = std::sync::Arc::clone(provider);
    std::sync::Arc::new(move || {
        current().map(|mut live| {
            live.auto_apply &= requested_auto_apply;
            live
        })
    })
}

pub(crate) fn check_with_settings(
    current_build: u64,
    provider: &crate::CheckSettingsProvider,
    force: bool,
) -> crate::UpdateStatus {
    check_recording(current_build, provider, force).0
}

/// [`check_with_settings`], and what the record made of the pass. The record counts a
/// failure only once the save after the check lands, so a failure before it (the pre-I/O
/// save refused by a full or read-only filesystem, a record or settings that will not
/// read) is counted by the background loop itself ([`crate::linux_notice::Record::Lost`]).
fn check_recording(
    current_build: u64,
    provider: &crate::CheckSettingsProvider,
    force: bool,
) -> (crate::UpdateStatus, crate::linux_notice::Record, bool) {
    check_recording_in(context(), current_build, provider, force)
}

/// [`check_recording`] against `context`, so a test drives the loop's own pass on a
/// fixture copy.
fn check_recording_in(
    context: Result<&Context, String>,
    current_build: u64,
    provider: &crate::CheckSettingsProvider,
    force: bool,
) -> (crate::UpdateStatus, crate::linux_notice::Record, bool) {
    let mut recorded = false;
    let mut took_lock = false;
    let attempt = (|| -> Result<(), String> {
        let settings = provider().ok_or("current update settings could not be read")?;
        let source = &settings.source;
        let context = context.clone()?;
        let mut state = context.read_state()?.ok_or(ENABLE_REMEDY)?;
        if !state.enabled {
            return Err(ENABLE_REMEDY.into());
        }
        let _lock = context.lock()?;
        took_lock = true;
        state = context.read_state()?.ok_or(MISSING_ENROLLMENT)?;
        let started = now()?;
        if !force && !automatic_check_due(&state, source, started) {
            return Ok(());
        }
        let checked_build = state.check_build;
        state.check_owner = source.owner.clone();
        state.check_repo = source.repo.clone();
        state.check_build = current_build;
        state.last_attempt_unix = started;
        state.next_check_unix = started.saturating_add(1800);
        context.save(&state)?;
        let apply_provider = request_apply_provider(provider, settings.auto_apply);
        let result = check_locked(context, &mut state, source, &apply_provider);
        match &result {
            Ok(Checked::Nothing) => state.check_build = checked_build,
            Ok(Checked::Channel) => {
                state.updated_at = aterm_types::rfc3339::format_rfc3339(now()? as u64);
                state.failing_checks = 0;
                state.failing_kind.clear();
            }
            Err(error) => {
                state.updated_at = aterm_types::rfc3339::format_rfc3339(now()? as u64);
                state.outcome = error.clone();
                state.failing_checks = state.failing_checks.saturating_add(1);
                state.failing_kind = "linux-update".into();
                let delay = if aterm_update_core::download_error_is_rate_limit(error) {
                    3600
                } else {
                    1800i64.saturating_mul(1i64 << state.failing_checks.min(4))
                };
                state.next_check_unix = started.saturating_add(delay);
            }
        }
        context.save(&state)?;
        recorded = true;
        result.map(|_| ())
    })();
    let record = match &attempt {
        Err(error) if !recorded && (error == LOCK_BUSY || error == MISSING_ENROLLMENT) => {
            crate::linux_notice::Record::Busy
        }
        Err(error) if !recorded && error != ENABLE_REMEDY => crate::linux_notice::Record::Lost,
        _ => crate::linux_notice::Record::Kept,
    };
    let mut status = status_in(context, current_build);
    if let Err(error) = attempt {
        // A typed check's reason reaches the log its trouble line points at; the
        if force {
            aterm_log::warn!("aterm-update: manual update check failed: {error}");
        }
        status.outcome = error;
        status.failing_checks = status.failing_checks.max(1);
        status.failing_kind = "linux-update".into();
    }
    (status, record, took_lock)
}

pub fn status(current_build: u64) -> crate::UpdateStatus {
    status_in(context(), current_build)
}

/// [`status`] of the copy `context` names.
fn status_in(context: Result<&Context, String>, current_build: u64) -> crate::UpdateStatus {
    let mut output =
        crate::UpdateStatus::empty(crate::enabled(), current_build, ENABLE_REMEDY.into());
    match context.and_then(|context| context.read_state().map(|state| (context, state))) {
        Ok((context, Some(state))) => {
            let linux = linux_status(context, &state);
            let rejected_stage = state
                .staged
                .as_ref()
                .filter(|_| linux.staged_build.is_none())
                .map(|stage| (stage.version.clone(), stage_admissible(&state, stage)));
            output.linux = Some(linux);
            output.enabled &= state.enabled;
            output.installable = true;
            output.outcome = state.outcome;
            if let Some((version, admissible)) = rejected_stage
                && state.failing_checks == 0
            {
                output.outcome = if admissible {
                    format!(
                        "The downloaded aterm {version} is missing or changed \u{2014} the next \
                         check downloads it again"
                    )
                } else {
                    format!("The downloaded aterm {version} is no longer allowed to install")
                };
            }
            output.updated_at = state.updated_at;
            output.failing_checks = state.failing_checks;
            output.failing_kind = state.failing_kind.clone();
            output.failing_checks_kind = state.failing_kind;
            output.failing_persistent = state.failing_checks >= crate::PERSISTENT_AFTER;
            if state.installed.build > current_build {
                output.outcome.push_str(&format!(
                    "; running build {current_build}, installed build {}",
                    state.installed.build
                ));
            }
            if !output.enabled {
                output.outcome = ENABLE_REMEDY.into();
            }
        }
        Ok((_, None)) => output.enabled = false,
        Err(error) => {
            output.enabled = false;
            output.outcome = error;
        }
    }
    output
}

fn linux_status(context: &Context, state: &State) -> crate::LinuxUpdateStatus {
    let staged = state
        .staged
        .as_ref()
        .filter(|stage| stage_present(context, state, stage));
    crate::LinuxUpdateStatus {
        installed_build: state.installed.build,
        installed_version: state.installed.version.clone(),
        staged_build: staged.map(|stage| stage.build),
        staged_version: staged.map(|stage| stage.version.clone()),
        staged_commit: staged.map(|stage| stage.commit.clone()),
        trial_phase: state
            .trial
            .as_ref()
            .map(|trial| format!("{:?}", trial.phase)),
        trial_starts: state.trial.as_ref().map_or(0, |trial| trial.starts),
        trial_healthy: state.trial.as_ref().is_some_and(|trial| trial.healthy),
        refused_newer: state.rejected_build > state.installed.build,
    }
}

/// The crash loops' ends no window has been told of yet ([`State::unsaid_trial_ends`]),
/// oldest first, taken under the update lock so that one check loop says each for the
/// copy.
fn take_unsaid_trial_ends(context: Result<&Context, String>) -> Vec<String> {
    (|| {
        let context = context.ok()?;
        if context.read_state().ok()??.unsaid_trial_ends.is_empty() {
            return None;
        }
        let _lock = context.lock().ok()?;
        let mut state = context.read_state().ok()??;
        let said = std::mem::take(&mut state.unsaid_trial_ends);
        context.save(&state).ok()?;
        Some(said)
    })()
    .unwrap_or_default()
}

/// Tell the window what one pass of the loop saw ([`crate::linux_notice::Reporter`]):
/// the check's `status`, `record` and `took_lock`, and the crash loops' ends the copy's
/// record keeps unsaid.
fn report_pass(
    reporter: &mut crate::linux_notice::Reporter,
    context: Result<&Context, String>,
    (status, record, took_lock): (crate::UpdateStatus, crate::linux_notice::Record, bool),
    on_checked: Option<&crate::CheckedNotify>,
    notify: Option<&crate::HealthNotify>,
) {
    let trial_ends = if notify.is_some() {
        take_unsaid_trial_ends(context)
    } else {
        Vec::new()
    };
    reporter.report(
        crate::linux_notice::Pass::of(&status, record, took_lock, &trial_ends),
        on_checked,
        notify,
    );
}

fn stage_present(context: &Context, state: &State, stage: &Identity) -> bool {
    stage_admissible(state, stage)
        && context
            .candidate_digest()
            .is_some_and(|hash| hash == stage.sha256)
}

fn stage_admissible(state: &State, stage: &Identity) -> bool {
    stage.build > state.high_water
        && stage.build > state.rejected_build
        && stage.build >= state.min_build
        && !stage
            .machine_id
            .as_ref()
            .is_some_and(|machine| state.revoked_machines.contains(machine))
}

/// Whether this copy is enrolled, so its background check loop runs: the small state
/// record alone.
fn enrolled_in(context: &Context) -> bool {
    context.read_state().is_ok_and(|state| state.is_some())
}

pub fn last_check_at(current_build: u64, source: &Source) -> Option<String> {
    let state = context().ok()?.read_state().ok()??;
    (state.check_owner == source.owner
        && state.check_repo == source.repo
        && state.check_build == current_build
        && !state.updated_at.is_empty())
    .then_some(state.updated_at)
}

pub(crate) fn spawn_background_check_with_settings(
    current_build: u64,
    provider: crate::CheckSettingsProvider,
    notify: Option<crate::HealthNotify>,
    on_checked: Option<crate::CheckedNotify>,
) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if !crate::automatic()
        || !context().is_ok_and(enrolled_in)
        || STARTED.swap(true, Ordering::AcqRel)
    {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("aterm-linux-update".into())
        .spawn(move || {
            let started = now().map_or_else(
                |_| String::new(),
                |unix| aterm_types::rfc3339::format_rfc3339(unix as u64),
            );
            let mut reporter = crate::linux_notice::Reporter::new(started);
            loop {
                report_pass(
                    &mut reporter,
                    context(),
                    check_recording(current_build, &provider, false),
                    on_checked.as_ref(),
                    notify.as_ref(),
                );
                std::thread::sleep(Duration::from_secs(crate::linux_notice::PASS_SECS));
            }
        });
}

#[cfg(test)]
mod tests;
