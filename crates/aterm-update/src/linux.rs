// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Opt-in, signed Linux single-executable updates. A disk replacement never execs,
//! signals, or closes a running terminal. The next ordinary launch uses the new
//! inode. The old inode and a durable transaction record remain available for
//! rollback until the replacement has demonstrated a healthy start: a window's
//! ([`boot`], then [`confirm`] after its first frame) or an interactive terminal
//! session's ([`session_started`], then [`confirm_session`] at its shell's first
//! output). Each lane proves its own: a session's prompt does not vouch for a window
//! (`Trial::window_proven`).

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use aterm_update_core::linux::LinuxTarget;

use crate::floor_sources::FloorSources;
use crate::linux_trial::{Lane, LaneConfirm, Lanes};
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
const LOCK_BUSY: &str = "Another aterm is updating this copy right now; try again when it finishes";
/// What a locked pass says when the record it found enrolled is gone under the lock.
const MISSING_ENROLLMENT: &str = "missing enrollment";

/// A replaced executable waits for one start of it to confirm it ([`confirm`]): a
/// window's or a terminal session's, whichever comes first. It asked for a window
/// until 2026-09-28, which a copy used only from the terminal never opens.
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

#[derive(Clone, Debug)]
struct Context {
    target: PathBuf,
    dir: PathBuf,
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
    /// When the latest counted start began (Unix seconds; `0` for none). A start over
    /// the budget rolls back only once this one can no longer confirm itself
    /// ([`crate::linux_trial::start_rolls_back`]): a burst of session starts is all
    /// counted before the first prompt confirms. Optional on the wire, so a record an
    /// older build wrote reads as "no start stamped", today's rule, and an older build
    /// reading this one ignores it.
    #[serde(default)]
    last_start_unix: i64,
    /// A WINDOW start of this trial confirmed it. Until one has, window starts go
    /// on counting, and roll the install back past the budget, even once a terminal
    /// session confirmed the trial for checks and applies: a session's prompt proves
    /// the session lane only (the round-four review; the rule is
    /// [`crate::linux_trial::Lanes`]).
    ///
    /// Optional on the wire, and absent means NOT proven, the careful reading: a
    /// running 0.98 saves the whole record without this field (it is a writer of
    /// this file, not only a reader), and a window that works then proves the lane
    /// again. Round four's `window_owed` and `window_pending` read "nothing owed"
    /// when absent, so one such save kept, for good, a build whose window crashes
    /// (round six); a record that still carries them has them ignored.
    #[serde(default)]
    window_proven: bool,
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
    /// Who raised [`Self::min_build`], so a revocation takes back what its machine
    /// raised (round seven, H1 finding 2). Absent (a record an older build wrote) ⇒
    /// all of it unattributed: the floor as it was, never lower.
    #[serde(default, skip_serializing_if = "FloorSources::is_empty")]
    min_build_sources: FloorSources,
    installed: Identity,
    trial: Option<Trial>,
    #[serde(default)]
    staged: Option<Identity>,
    rejected_build: u64,
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

/// Injected only through private functions in unit tests. Production callers
/// always use these committed anchors; no environment or enrollment file can
/// replace the authority.
///
/// The paper master is the ONE anchor (K1 retired, 2026-09-23): an appcast is
/// authorized by a machine the master-signed roster admits, never by a bare channel
/// key, so a build that pins no master verifies no update channel at all.
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
    LinuxTarget::native().ok_or_else(|| "this Linux architecture has no update target".into())
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
        // Walk from the root DOWN. An owner-only ancestor excludes every other
        // user from descendant traversal, so a 0775 lib below ~/.local (0700)
        // is safe. A writable ancestor ABOVE that boundary (including /tmp)
        // can rename the boundary itself and is always refused.
        let mut private_ancestor = false;
        for ancestor in parent.ancestors().collect::<Vec<_>>().into_iter().rev() {
            let md = fs::symlink_metadata(ancestor).map_err(|e| e.to_string())?;
            if !md.is_dir()
                || ![0, uid()].contains(&md.uid())
                || (!private_ancestor && md.mode() & 0o022 != 0)
            {
                let why = if md.file_type().is_symlink() {
                    "is a symlink"
                } else if !md.is_dir() {
                    "isn\u{2019}t a directory"
                } else if ![0, uid()].contains(&md.uid()) {
                    "belongs to another user"
                } else {
                    "is writable by other users"
                };
                return Err(format!("{} {why}", ancestor.display()));
            }
            private_ancestor |= md.uid() == uid() && md.mode() & 0o077 == 0;
        }
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
        })
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
    /// own read looked like a re-entrant `context` lock (2026-09-24).
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
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    let md = file.metadata().map_err(|e| e.to_string())?;
    if !md.is_file()
        || md.uid() != uid()
        || md.mode() & 0o022 != 0
        || md.len() > cap
        || (executable && (md.mode() & 0o111 == 0 || md.mode() & 0o6000 != 0))
    {
        return Err(format!("unsafe update file: {}", path.display()));
    }
    Ok(file)
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
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|e| format!("sync {}: {e}", path.display()))
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

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("update record has no parent")?;
    let (temp, mut file) = fresh_file(parent, "record")?;
    let result = (|| {
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&temp, path).map_err(|e| e.to_string())?;
        sync_dir(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = checked_file(path, BINARY_LIMIT, false)?;
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
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
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
    if commit.len() != 40
        || !commit.bytes().all(|b| b.is_ascii_hexdigit())
        || manifest.build_number == 0
    {
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
    use std::process::{Command, Stdio};
    let (output_path, output) = fresh_file(private_dir, "probe-output")?;
    let (error_path, error) = fresh_file(private_dir, "probe-error")?;
    let result = (|| {
        let mut child = Command::new(path)
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
            return Err(format!(
                "authenticated Linux candidate identity probe exited {status}"
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

/// Bootstrap and reinstallation use the very same destination lock, admission
/// floors, exact rollback inode and durable replacement journal as self-update.
/// The installer must never place the downloaded candidate before this call.
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
            // An un-enrolled binary is only an exact local rollback baseline. Do not
            // invent signed provenance or attribute the candidate's build to it.
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
    // Stage a private copy on the target filesystem. Never execute a mutable
    // download-directory path after authenticating a different private copy.
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
        file.sync_all().map_err(|e| e.to_string())?;
        verify_binary(&temp, &manifest, native()?).map(|_| ())
    })();
    // Linux refuses exec with ETXTBSY while ANY writable descriptor remains.
    drop(file);
    if let Err(error) = copied {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    let candidate = context.dir.join("candidate");
    fs::rename(&temp, &candidate).map_err(|e| e.to_string())?;
    sync_dir(&context.dir)?;
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
        // On a first install there is no prior executable to lose. Write the
        // authenticated disabled receipt BEFORE rename; a retry may recover
        // either an absent target or the exact new inode without lowering floors.
        state.installed = identity;
        state.high_water = state.high_water.max(state.installed.build);
        context.save(&state)?;
        if !exists {
            fs::rename(&candidate, &context.target).map_err(|e| e.to_string())?;
            sync_dir(context.target.parent().ok_or("missing target parent")?)?;
        }
    }
    state.enabled = true;
    // A first install's version and path are the installer's own line.
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
            sync_dir(dir)?;
        }
        Ok(())
    }
}

fn now() -> Result<i64, String> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "system clock predates the epoch".to_string())
        .and_then(|duration| i64::try_from(duration.as_secs()).map_err(|e| e.to_string()))
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
    // A REVOKED MACHINE'S FLOOR GOES WITH IT (round seven, H1 finding 2). A stolen
    // key's appcast with `min_build = 9_999_999_999` raised the floor below, and the
    // revocation left it there: every later genuine release was refused as "below the
    // signed minimum-build floor" for good. What the machines still trusted (and every
    // unattributed record) asked for stands.
    (state.min_build, state.min_build_sources) =
        state
            .min_build_sources
            .step(state.min_build, &state.revoked_machines, 0, None);
    // Observation is durable even if the admitted roster revokes this appcast's
    // signer. Never retry a revoked signer under an older roster afterwards.
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

/// Explicit enrollment is separate from merely running a dev checkout. A release
/// installer supplies its proof directory; an explicit local developer enrollment
/// trusts only the current local bytes as the rollback baseline, not as a release.
pub fn enable(current_build: u64, proof_dir: Option<&Path>) -> Result<String, String> {
    native()?;
    let context = context()?;
    let _lock = context.lock()?;
    let baseline = Identity {
        build: current_build,
        sha256: hash_file(&context.target)?,
        version: aterm_types::version::APP_VERSION.into(),
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
    // Until its rename commits, a prepared install leaves `installed` naming the
    // file it replaces — which is not always its rollback target (`trial.old`):
    // an install over a build whose window never confirmed rolls back past it
    // ([`crate::linux_trial::installed_window_unproven`]).
    let still_installed = digest == state.installed.sha256;
    match trial.phase {
        Phase::Prepared if digest == trial.new.sha256 => {
            trial.phase = Phase::Installed;
            state.installed = trial.new.clone();
            state.high_water = state.high_water.max(trial.new.build);
            state.staged = None;
            state.outcome = installed_pending(&trial.new.version);
            context.save(state)
        }
        Phase::Prepared if still_installed => {
            // No replacement happened. Abort this intent so the normal
            // bootstrap entry can retry even when the old executable predates
            // Linux update verbs. Admitted policy floors and backup stay intact.
            state.trial = None;
            state.outcome = "recovered an interrupted pre-replacement transaction; the original executable remains installed".into();
            context.save(state)
        }
        Phase::RollbackPrepared if digest == trial.old.sha256 => {
            state.installed = trial.old.clone();
            state.rejected_build = state.rejected_build.max(trial.new.build);
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
    // What this install rolls back TO: the installed build, unless a window start
    // of it was counted and never confirmed — then the installed build's own
    // rollback target, whose copy that trial kept
    // ([`crate::linux_trial::installed_window_unproven`]).
    // The trial's target is inherited only while it is still a rollback it may take
    // ([`rollback_refusal`], asked after `authorize` has ratcheted the floor and the
    // revocations): a target a yank or a revocation has since ruled out, or whose
    // kept copy is gone or changed, would leave this install with no rollback at all,
    // where the installed build — proven for sessions, and permitted — is one.
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
        // The installed build's copy is the executable itself, whose identity was
        // checked above. A stale name in its place that is not that build — a file
        // changed or replaced since an earlier install kept it — refused every apply
        // from here on; it is set aside, and the executable linked in its place.
        let stale = match fs::symlink_metadata(&backup) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("read {}: {error}", backup.display())),
            Ok(meta) => Some(!meta.is_file() || hash_file(&backup)? != old.sha256),
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
    checked_file(&backup, BINARY_LIMIT, true)?
        .sync_all()
        .map_err(|e| e.to_string())?;
    sync_dir(&context.dir)?;
    checkpoint(Checkpoint::Backup)?;
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())?;
    checked_file(&candidate, BINARY_LIMIT, true)?
        .sync_all()
        .map_err(|e| e.to_string())?;
    state.trial = Some(Trial {
        old,
        new: identity.clone(),
        phase: Phase::Prepared,
        starts: 0,
        healthy: false,
        last_start_unix: 0,
        window_proven: false,
    });
    context.save(state)?;
    checkpoint(Checkpoint::Prepared)?;
    // Atomic single-path replacement: the target always names a complete old or
    // complete new executable. Running processes keep their old inode and PTYs.
    fs::rename(&candidate, &context.target)
        .map_err(|e| format!("replace Linux executable: {e}"))?;
    sync_dir(
        context
            .target
            .parent()
            .ok_or("missing executable directory")?,
    )?;
    checkpoint(Checkpoint::Replaced)?;
    state
        .trial
        .as_mut()
        .ok_or("missing prepared transaction")?
        .phase = Phase::Installed;
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
        return Err(format!("{refusal}; refusing"));
    }
    if hash_file(&context.target)? != trial.new.sha256
        || hash_file(&context.backup(&trial.old))? != trial.old.sha256
    {
        return Err("rollback current/previous identities could not be proved".into());
    }
    let (temp, mut file) = fresh_file(&context.dir, "restore")?;
    let mut previous = checked_file(&context.backup(&trial.old), BINARY_LIMIT, true)?;
    std::io::copy(&mut previous, &mut file).map_err(|e| e.to_string())?;
    file.set_permissions(fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    if hash_file(&temp)? != trial.old.sha256 {
        return Err("rollback copy changed".into());
    }
    // Close the last writer before making this inode executable by new launches.
    drop(file);
    state
        .trial
        .as_mut()
        .ok_or("missing rollback transaction")?
        .phase = Phase::RollbackPrepared;
    context.save(state)?;
    fs::rename(&temp, &context.target).map_err(|e| e.to_string())?;
    after_replace(&context.target)?;
    sync_dir(
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
    state.installed = trial.old;
    state.rejected_build = state.rejected_build.max(trial.new.build);
    state.trial = None;
    context.save(state)?;
    prune_backups(context, state);
    Ok(())
}

/// Why `trial` can never be rolled back, or `None` when nothing rules it out: the
/// build it would restore is below the signed minimum-build floor (a yank), its
/// signer was revoked, or its kept copy is gone or no longer that build. Each is a
/// fact about the record, not a passing failure, so the answer stays the same on
/// every later try; a copy that cannot be READ is an error, tried again.
fn rollback_refusal(
    context: &Context,
    state: &State,
    trial: &Trial,
) -> Result<Option<&'static str>, String> {
    Ok(if trial.old.build < state.min_build {
        Some("rollback is below the signed minimum-build floor")
    } else if trial
        .old
        .machine_id
        .as_ref()
        .is_some_and(|machine| state.revoked_machines.contains(machine))
    {
        Some("rollback signer was revoked by an admitted machine roster")
    } else {
        let backup = context.backup(&trial.old);
        match fs::symlink_metadata(&backup) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Some("the previous copy kept for rollback is gone")
            }
            Err(error) => return Err(format!("read {}: {error}", backup.display())),
            // Left to the rollback, a copy that is not that build failed "could not be
            // proved" at every start past the budget while checks, applies and
            // reinstalls all waited on the trial, for good (round six).
            Ok(meta) if !meta.is_file() || hash_file(&backup)? != trial.old.sha256 => {
                Some("the previous copy kept for rollback is not that build")
            }
            Ok(_) => None,
        }
    })
}

/// Remove every kept rollback copy but the one the trial on record can restore
/// (`rollback-<its old build's digest>`). Each apply hard-links the executable it
/// replaces into the update directory, and that link is then the only name of the
/// replaced file, so a copy nothing can restore any more held a whole executable
/// on disk for every update of the copy's life, with nothing to report or remove
/// it (round six). Best effort: a copy that cannot be removed now is removed after
/// the next update, and a failure here decides nothing.
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
    // The last replacement renamed its candidate onto the executable: until a start of
    // it confirms it, that install is the answer, not an empty download slot.
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

/// A WINDOW of this executable is starting (the GUI entry's boot lane): count it
/// against a pending trial, and roll the trial back once it has spent its budget
/// ([`boot_locked`]).
pub fn boot(current_build: u64, current_commit: &str) -> crate::ApplyOutcome {
    let result = (|| -> Result<(), String> {
        let context = context()?;
        if context.read_state()?.is_none() {
            return Ok(());
        }
        let _lock = context.lock()?;
        let mut state = context.read_state()?.ok_or(MISSING_ENROLLMENT)?;
        let actual = fs::metadata("/proc/self/exe").map_err(|e| e.to_string())?;
        boot_locked(
            context,
            &mut state,
            current_build,
            current_commit,
            (actual.dev(), actual.ino()),
            now()?,
        )
        .map(|_| ())
    })();
    match result {
        Ok(()) => crate::ApplyOutcome::NoUpdate,
        Err(error) => crate::ApplyOutcome::Deferred(error),
    }
}

/// An interactive TERMINAL SESSION of this executable is starting (`aterm`'s session
/// lane): the same count as a window's [`boot`], and `true` when this launch is a start
/// of the pending trial, so the session confirms it ([`confirm_session`]) once it proves
/// healthy.
///
/// Until 2026-09-28 only a window counted or confirmed. A copy used only from the
/// terminal installed one update from its session lane's check, and then nothing ever
/// confirmed it: every later check stopped at "waits for a window", for good, and
/// nothing rolled it back either.
///
/// The steady state — no trial, or a confirmed one — costs one small unlocked read:
/// the locked path hashes the whole executable ([`recover`]), which a new tab must not
/// pay at every launch for the life of an install. A copy that cannot update itself
/// (no enrollment, or a directory this user does not own) has no trial, so it says
/// nothing; a failure once a trial is pending is a log line, and that start is simply
/// not counted.
///
/// A `--headless` instance starts through here too, not through [`boot`]: it opens
/// no window, so it proves what a session proves, and its [`confirm`] then confirms
/// the session lane. Counted and confirmed as a window, one headless instance binding
/// its socket settled the window lane, and a build whose real windows crash was kept
/// for good (round six).
pub fn session_started(current_build: u64, current_commit: &str) -> bool {
    SESSION_LANE.store(true, Ordering::Release);
    let Ok(context) = context() else {
        return false;
    };
    now()
        .and_then(|now_unix| {
            session_started_in(
                context,
                current_build,
                current_commit,
                proc_self_inode,
                now_unix,
            )
        })
        .unwrap_or_else(|error| {
            aterm_log::warn!(
                "aterm-update: this session's start of the installed update did not finish: {error}"
            );
            false
        })
}

/// This process started in the SESSION lane ([`session_started`]): a terminal
/// session, or a `--headless` instance, whose [`confirm`] confirms that lane and
/// never the window's.
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

/// [`session_started`] against `context`, so a test drives the session lane's own
/// path — the unlocked steady-state read, the lock, the count — on a fixture copy.
/// `actual_inode` is asked only once a trial is pending.
fn session_started_in(
    context: &Context,
    current_build: u64,
    current_commit: &str,
    actual_inode: impl FnOnce() -> Result<(u64, u64), String>,
    now_unix: i64,
) -> Result<bool, String> {
    let pending = context
        .read_state()?
        .and_then(|state| state.trial)
        .is_some_and(|trial| !trial.healthy);
    if !pending {
        return Ok(false);
    }
    let _lock = context.lock()?;
    let mut state = context.read_state()?.ok_or(MISSING_ENROLLMENT)?;
    count_start_locked(
        context,
        &mut state,
        Lane::Session,
        current_build,
        current_commit,
        actual_inode()?,
        now_unix,
    )
}

/// A WINDOW start counted against a pending trial ([`count_start_locked`]).
fn boot_locked(
    context: &Context,
    state: &mut State,
    current_build: u64,
    current_commit: &str,
    actual_inode: (u64, u64),
    now_unix: i64,
) -> Result<bool, String> {
    count_start_locked(
        context,
        state,
        Lane::Window,
        current_build,
        current_commit,
        actual_inode,
        now_unix,
    )
}

/// Count one start of this executable, from `lane`, against a pending trial, at
/// `now_unix`; `true` when this process is a start of that trial (and should confirm
/// it). A process running some other file than the one on trial — an old session's
/// inode — counts nothing. Past the budget the trial rolls back, but only once the
/// budget's last start can no longer confirm itself
/// ([`crate::linux_trial::start_rolls_back`], [`crate::linux_trial::start_stamp`]), so
/// a burst of session starts cannot roll back a healthy build before its first prompt,
/// and a crash loop however fast is rolled back within one settle window of it.
///
/// A trial is pending for a lane until that lane has proved itself: a session counts
/// only until the trial is confirmed, a window until a WINDOW confirmed it — a trial a
/// session confirmed still owes the window's verdict ([`Trial::window_proven`]). A
/// trial no rollback can ever be taken from ([`rollback_refusal`]) ends instead, keeping
/// the build, so nothing waits on it for good.
fn count_start_locked(
    context: &Context,
    state: &mut State,
    lane: Lane,
    current_build: u64,
    current_commit: &str,
    actual_inode: (u64, u64),
    now_unix: i64,
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
    let previous_start = trial.last_start_unix;
    trial.starts = trial.starts.saturating_add(1);
    if crate::linux_trial::stamp_lost(trial.starts, previous_start) {
        // An older build's save dropped the stamp mid-trial: this start (one of the
        // first `LOST_STAMP_SPARES` past the budget) stamps its own time and is
        // judged no further, so a burst of healthy session starts the save landed
        // in is not rolled back before its first prompt. A stamp lost before every
        // start — a running 0.98 drops it at each save — is spared at most
        // `LOST_STAMP_SPARES` times; the start after that rolls back.
        trial.last_start_unix = now_unix;
        context.save(state)?;
        return Ok(true);
    }
    trial.last_start_unix = crate::linux_trial::start_stamp(trial.starts, previous_start, now_unix);
    context.save(state)?;
    let Some(trial) = state.trial.clone().filter(|trial| {
        crate::linux_trial::start_rolls_back(trial.starts, previous_start, now_unix)
    }) else {
        return Ok(true);
    };
    let (restored, new) = (restored_name(&trial.old), &trial.new.version);
    if let Some(refusal) = rollback_refusal(context, state, &trial)? {
        // No rollback can ever be taken, so the trial is over: the build stays, and
        // checks and applies go on so a newer release can replace it. Left standing,
        // this refusal came back at every later start, while checks, applies,
        // reinstalls and `aterm update rollback` all waited on the trial, for good
        // (round six).
        state.trial = None;
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

/// A start of this executable reached its healthy checkpoint in the GUI entry — a
/// window's first frame, or a `--headless` instance's bound socket: confirm its
/// trial ([`confirm_lane_locked`]) for the lane its start was counted in
/// ([`confirm_lane`]: a headless instance starts, and so confirms, as a session).
/// `true` when nothing more is owed by this process.
///
/// A trial this lane has nothing to prove to costs one small unlocked read: the
/// locked path hashes the whole executable ([`recover`]), which every window of a
/// settled install used to pay for a confirmation nothing waited on (round six).
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
        // /proc/self/exe refers to this process's actual inode, not whichever
        // executable a sibling update most recently placed at the install path.
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

/// An interactive TERMINAL SESSION of this executable proved healthy (its shell's
/// first output, or [`crate::LINUX_SESSION_HEALTHY_AFTER`] alive): confirm its trial
/// for the session lane. The window lane goes on proving itself
/// ([`Trial::window_proven`]).
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

/// A WINDOW's confirmation ([`confirm_lane_locked`]): `false` only for a process
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

/// Confirm the pending trial from `lane`. Every lane needs the trial's own build and
/// commit and this process running the very file on trial. A window's confirmation
/// settles every lane. A session's settles the trial for checks and applies, and the
/// window lane still owes its verdict, from a clean count ([`Trial::window_proven`]) —
/// so a build that cannot open a window is rolled back by its window starts whichever
/// lane was used first. A lane already proved answers [`Confirmed::NotPending`] and
/// writes nothing: every window of a settled install used to rewrite the recorded
/// outcome, a failing check's reason included (round six).
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
    // The lane still owing a verdict counts its own starts: a burst of session
    // starts spent the shared budget, and the first window after it must not roll
    // back a build it has not yet tried.
    trial.starts = 0;
    trial.last_start_unix = 0;
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
        aterm_update_core::download_bytes(&url, None, limit)
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

/// The channel head, authenticated — and the held stage proved again under whatever
/// roster that authentication ADMITTED, whatever verdict the head itself gets (round
/// seven, H1 finding 34, second exit).
///
/// `authorize_one` admits the head's roster and saves the ratcheted `roster_floor`
/// BEFORE it judges the head's appcast, its signer's durable revocation, its
/// attribution, the policy ordering and the tag. A head refused at any of those steps
/// — the stolen-key case: a revoked machine's release carrying the owner's public
/// revoking roster; or a tag that does not match its appcast — still moved the floor,
/// and returning at `?` before the reroot left the held stage's roster-1 proof refused
/// as `Rollback` by `aterm update apply`. The reroot reads only the head's roster and
/// its signature, so it runs on both arms; a roster that was never admitted proves
/// nothing and leaves the saved proof as it was. The head's own refusal is still the
/// answer the check reports.
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
        // A source-only/non-APP head is allowed ONLY as a location to test for
        // missing appcast. If it carries an appcast, authenticate_tag still
        // rejects the noncanonical tag instead of silently falling through.
        Err(aterm_update_core::pointer::PointerError::OtherTag { tag }) => Ok(tag),
        Err(error) => Err(error.to_string()),
    }
}

fn check_locked(
    context: &Context,
    state: &mut State,
    source: &Source,
    provider: &crate::CheckSettingsProvider,
) -> Result<(), String> {
    recover(context, state)?;
    if state
        .trial
        .as_ref()
        .is_some_and(|trial| trial.phase == Phase::Installed && !trial.healthy)
    {
        state.outcome = format!(
            "aterm {} is installed \u{2014} the next aterm you start finishes it; update checks \
             wait until then",
            state.installed.version
        );
        return context.save(state);
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
    // The channel is read on the release download host alone — the pointer, then the
    // head's own tag-specific assets — and never listed through the GitHub API (owner
    // ruling R3). A head that is not an app release YET (its appcast answers 404) or that
    // carries no executable for this architecture has nothing to install: a healthy,
    // recorded outcome, and the next check reads the head again. The cut owns the head
    // (it moves `latest` only to a release carrying its app assets), so neither state
    // outlasts a cut that ships this target.
    let proof = match download_proof(source, &head, None) {
        Ok(proof) => proof,
        Err(error) if error.starts_with("missing appcast: ") => {
            state.outcome =
                format!("channel head {head} has no app manifest yet — nothing to install from it");
            return context.save(state);
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
        return context.save(state);
    }
    let artifact = manifest
        .linux_artifact(target)?
        .ok_or("elected Linux artifact disappeared")?;
    if manifest.build_number < state.min_build {
        return Err("the published Linux build is below the signed minimum-build floor; no install permitted".into());
    }
    if manifest.build_number <= state.high_water || manifest.build_number <= state.rejected_build {
        // A rollback leaves `high_water` at the refused build, so "up to date" is
        // measured against what is installed.
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
        return context.save(state);
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
        let downloaded = aterm_update_core::download_to(&url, None, &temp, artifact.size)
            .and_then(|()| verify_binary(&temp, &manifest, target).map(|_| ()));
        if let Err(error) = downloaded {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }
        clear_stage(context, state)?;
        fs::rename(&temp, context.dir.join("candidate")).map_err(|e| e.to_string())?;
        sync_dir(&context.dir)?;
    }
    // Even a byte-identical existing stage receives CURRENT proof and policy;
    // finish_candidate repeats authorization, digest and compiled-identity checks.
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
    Ok(())
}

/// A HELD STAGE IS PROVED AGAIN UNDER THE ROSTER THIS CHECK ADMITTED (round seven,
/// H1 finding 34).
///
/// `authorize_one` ratchets `roster_floor` on every check, including a check of a head
/// that has nothing to install (no app manifest yet, no executable for this target),
/// while the held stage keeps the proof it was staged with. `apply` then re-authorizes
/// that saved proof, whose roster is now older than the floor, and the replay defence
/// refused it — "machine roster admission refused: Rollback" — for a stage every
/// surface still showed as downloaded, until a later head restaged.
///
/// The head's roster is master-verified and admitted, so the stage's own appcast is
/// judged again under it: a signer that roster still lists is proved again and the
/// saved proof carries the newer roster; one it revoked is not re-proved, and the
/// stage is left to the revocation gates that already refuse it.
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
        // Not this stage's proof, or not provable under the newer roster: the saved
        // proof stays as it was, and the gates that judge it say why.
        Ok(_) | Err(_) => Ok(()),
    }
}

fn reusable_stage(
    context: &Context,
    state: &State,
    manifest: &Manifest,
    target: LinuxTarget,
) -> bool {
    state.staged.as_ref().is_some_and(|stage| {
        stage_admissible(state, stage)
            && stage.build == manifest.build_number
            && verify_binary(&context.dir.join("candidate"), manifest, target)
                .is_ok_and(|identity| identity.sha256 == stage.sha256)
    })
}

fn automatic_apply_allowed(source: &Source, provider: &crate::CheckSettingsProvider) -> bool {
    let Some(current) = provider() else {
        return false;
    };
    // Settings are the one policy surface. The provider admits the current
    // persisted/live snapshot; retired environment knobs grant no authority.
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

/// Why [`automatic_apply_allowed`] holds a stage, read from the same provider.
fn apply_held_reason(source: &Source, provider: &crate::CheckSettingsProvider) -> &'static str {
    match provider() {
        None => "the update settings couldn\u{2019}t be confirmed",
        Some(current) if current.source != *source => "the update channel changed during the check",
        Some(current) if !current.auto_apply => "`[update] auto_apply` is off",
        Some(_) => "the update settings changed during the check",
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
        return held_stage(context, state, apply_held_reason(source, provider));
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
        // Prepared intent was durable, but the exact old executable is still
        // installed. Ordinary recovery abandons that intent without dropping
        // any policy floor or authenticated candidate.
        recover(context, state)?;
        return held_stage(context, state, apply_held_reason(source, provider));
    }
    result
}

fn automatic_check_due(state: &State, source: &Source, started: i64) -> bool {
    // Old and new executable mappings intentionally coexist after replacement.
    // Their alternating compiled build numbers must not reset the ONE install's
    // shared network cooldown. The display receipt still records its exact build.
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

/// [`check_with_settings`], and what the record made of the pass: one that FAILED
/// WITHOUT RECORDING it is counted by the caller, since the
/// record's `failing_checks` counts a failure only once the save after the check
/// lands, so a failure before it — the pre-I/O save refused by a full or read-only
/// filesystem, a record that will not read, settings that will not — leaves the
/// count where it was, every pass, and the background loop counts those passes
/// itself ([`crate::linux_notice::Record::Lost`]). Updates switched off, or a
/// copy not enrolled, is the remedy's sentence, not a failing check; another aterm
/// holding the lock, or unenrolling the copy under it, is no check at all
/// ([`crate::linux_notice::Record::Busy`]).
fn check_recording(
    current_build: u64,
    provider: &crate::CheckSettingsProvider,
    force: bool,
) -> (crate::UpdateStatus, crate::linux_notice::Record) {
    let mut recorded = false;
    let attempt = (|| -> Result<(), String> {
        let settings = provider().ok_or("current update settings could not be read")?;
        let source = &settings.source;
        let context = context()?;
        let mut state = context.read_state()?.ok_or(ENABLE_REMEDY)?;
        if !state.enabled {
            return Err(ENABLE_REMEDY.into());
        }
        let _lock = context.lock()?;
        state = context.read_state()?.ok_or(MISSING_ENROLLMENT)?;
        let started = now()?;
        if !force && !automatic_check_due(&state, source, started) {
            return Ok(());
        }
        state.check_owner = source.owner.clone();
        state.check_repo = source.repo.clone();
        state.check_build = current_build;
        state.last_attempt_unix = started;
        // Before I/O: another window, or a crash mid-download, cannot immediately
        // repeat this attempt. The policy is shared across all enrolled processes.
        state.next_check_unix = started.saturating_add(1800);
        context.save(&state)?;
        // A request that began manual stays manual even if the config becomes
        // automatic mid-request. A request that began automatic can be vetoed.
        let apply_provider = request_apply_provider(provider, settings.auto_apply);
        let result = check_locked(context, &mut state, source, &apply_provider);
        state.updated_at = aterm_types::rfc3339::format_rfc3339(now()? as u64);
        match &result {
            Ok(()) => {
                state.failing_checks = 0;
                state.failing_kind.clear();
            }
            Err(error) => {
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
        result
    })();
    let record = match &attempt {
        Err(error) if !recorded && (error == LOCK_BUSY || error == MISSING_ENROLLMENT) => {
            crate::linux_notice::Record::Busy
        }
        Err(error) if !recorded && error != ENABLE_REMEDY => crate::linux_notice::Record::Lost,
        _ => crate::linux_notice::Record::Kept,
    };
    let mut status = status(current_build);
    if let Err(error) = attempt {
        // A typed check's reason reaches the log its trouble line points at; the
        // background loop logs each changed outcome itself.
        if force {
            aterm_log::warn!("aterm-update: manual update check failed: {error}");
        }
        status.outcome = error;
        status.failing_checks = status.failing_checks.max(1);
        status.failing_kind = "linux-update".into();
    }
    (status, record)
}

pub fn status(current_build: u64) -> crate::UpdateStatus {
    let mut output =
        crate::UpdateStatus::empty(crate::enabled(), current_build, ENABLE_REMEDY.into());
    match context().and_then(|context| context.read_state().map(|state| (context, state))) {
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
            // The outcome that announced the stage no longer holds; a failure's own
            // sentence stays.
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

fn stage_present(context: &Context, state: &State, stage: &Identity) -> bool {
    // This bounded disk check runs on updater/control workers, never the event
    // loop. The durable record alone cannot describe a deleted/tampered download.
    stage_admissible(state, stage)
        && hash_file(&context.dir.join("candidate")).is_ok_and(|hash| hash == stage.sha256)
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
) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if !crate::automatic()
        || !status(current_build).installable
        || STARTED.swap(true, Ordering::AcqRel)
    {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("aterm-linux-update".into())
        .spawn(move || {
            let mut logged = String::new();
            // Only a persistent failure, its changed reason and its recovery reach the
            // window (`crate::linux_notice`): every result used to arrive as a warning.
            let started = now().map_or_else(
                |_| String::new(),
                |unix| aterm_types::rfc3339::format_rfc3339(unix as u64),
            );
            let mut announcer = crate::linux_notice::Announcer::new(started);
            loop {
                let (status, record) = check_recording(current_build, &provider, false);
                if status.outcome != logged {
                    aterm_log::info!("aterm-update: {}", status.outcome);
                    logged.clone_from(&status.outcome);
                }
                let owed = announcer.tick(crate::linux_notice::Pass {
                    outcome: &status.outcome,
                    failing_checks: status.failing_checks,
                    updated_at: &status.updated_at,
                    record,
                });
                if let (Some(notify), Some((title, body))) = (&notify, owed) {
                    notify(title, body);
                }
                std::thread::sleep(Duration::from_secs(60));
            }
        });
}

#[cfg(test)]
mod tests;
