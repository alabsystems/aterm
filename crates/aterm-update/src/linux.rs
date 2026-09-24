// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Opt-in, signed Linux single-executable updates. A disk replacement never execs,
//! signals, or closes a running terminal. The next ordinary launch uses the new
//! inode. The old inode and a durable transaction record remain available for
//! rollback until the replacement has demonstrated a healthy GUI boot.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use aterm_update_core::linux::LinuxTarget;
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
const MAX_TRIAL_STARTS: u32 = 3;

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
#[derive(Clone, Copy)]
struct Pins<'a> {
    masters: &'a [&'a str],
    channels: &'a [&'a str],
}

const PRODUCTION_PINS: Pins<'static> = Pins {
    masters: aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
    channels: aterm_update_core::pins::UPDATE_CHANNEL_PUBKEYS,
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
                return Err(format!(
                    "unsafe update path component: {}",
                    ancestor.display()
                ));
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
            return Err("Linux update directory must be a real, owner-only directory".into());
        }
        aterm_update_core::ensure_private_dir(&self.dir).map_err(|e| e.to_string())
    }

    fn lock(&self) -> Result<FileLock, String> {
        self.prepare_dir()?;
        let path = self.dir.join("lock");
        if path.try_exists().map_err(|e| e.to_string())? {
            checked_file(&path, RECORD_LIMIT, false)?;
        }
        FileLock::acquire_within(&path, Duration::from_millis(500))
            .map_err(|e| format!("Linux update transaction is unavailable: {e}"))
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
        if state.schema != 1 || state.target != self.target {
            return Err("Linux update state belongs to a different install or schema".into());
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
    state.outcome = format!(
        "authenticated Linux release {} (build {}) installed and enrolled; existing sessions continue unchanged",
        state.installed.version, state.installed.build
    );
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
    let attribution = if pins.masters.is_empty() {
        crate::sig::verify_detached_any(pins.channels, &proof.appcast, &proof.signature)
            .map_err(|e| format!("Linux appcast signature refused: {e:?}"))?;
        None
    } else {
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
        // Observation is durable even if the admitted roster revokes this appcast's
        // signer. Never retry a revoked signer under an older roster afterwards.
        context.save(state)?;
        Some(
            roster
                .authorize_appcast(&proof.appcast, &proof.signature, unix)
                .map_err(|e| format!("Linux appcast authorization refused: {e:?}"))?,
        )
    };
    let text = std::str::from_utf8(&proof.appcast).map_err(|e| e.to_string())?;
    let manifest = Manifest::parse(text)?;
    if let Some(attribution) = attribution {
        if state.revoked_machines.contains(&attribution.machine_id) {
            return Err("appcast signer is in the durable revocation set".into());
        }
        attribution
            .bind(manifest.machine_id.as_deref(), manifest.roster_seq)
            .map_err(|e| format!("Linux appcast signer attribution refused: {e:?}"))?;
    }
    state.min_build = state.min_build.max(manifest.min_build.unwrap_or(0));
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
    state.outcome =
        "automatic signed Linux updates enabled; existing sessions are never restarted".into();
    context.save(&state)?;
    Ok(state.outcome)
}

fn recover(context: &Context, state: &mut State) -> Result<(), String> {
    let Some(trial) = state.trial.as_mut() else {
        return Ok(());
    };
    let digest = hash_file(&context.target)?;
    match trial.phase {
        Phase::Prepared if digest == trial.new.sha256 => {
            trial.phase = Phase::Installed;
            state.installed = trial.new.clone();
            state.high_water = state.high_water.max(trial.new.build);
            state.staged = None;
            context.save(state)
        }
        Phase::Prepared if digest == trial.old.sha256 => {
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
    if state
        .trial
        .as_ref()
        .is_some_and(|trial| trial.phase == Phase::Installed && !trial.healthy)
    {
        return Err("the installed update still awaits a healthy ordinary launch".into());
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
    let backup = context.backup(&state.installed);
    if !backup.try_exists().map_err(|e| e.to_string())? {
        fs::hard_link(&context.target, &backup)
            .map_err(|e| format!("preserve rollback inode: {e}"))?;
    }
    if hash_file(&backup)? != state.installed.sha256 {
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
        old: state.installed.clone(),
        new: identity.clone(),
        phase: Phase::Prepared,
        starts: 0,
        healthy: false,
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
    state.outcome = format!(
        "installed {} (build {}) on disk; existing sessions continue unchanged; new launches use it",
        state.installed.version, state.installed.build
    );
    state.failing_checks = 0;
    state.failing_kind.clear();
    context.save(state)?;
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
    if trial.old.build < state.min_build {
        return Err("rollback is below the signed minimum-build floor; refusing".into());
    }
    if trial
        .old
        .machine_id
        .as_ref()
        .is_some_and(|machine| state.revoked_machines.contains(machine))
    {
        return Err("rollback signer was revoked by an admitted machine roster; refusing".into());
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
    state.installed = trial.old;
    state.rejected_build = state.rejected_build.max(trial.new.build);
    state.trial = None;
    state.outcome =
        "restored the exact previous executable on disk; existing sessions were not restarted"
            .into();
    context.save(state)
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
    let mut state = context
        .read_state()?
        .ok_or("this executable is not enrolled for Linux updates")?;
    if !crate::enabled() || !state.enabled {
        return Err("Linux self-update enrollment is disabled".into());
    }
    let proof = Proof::read(&context.dir)?;
    apply_locked(context, &mut state, &proof, PRODUCTION_PINS)
}

pub fn boot(current_build: u64, current_commit: &str) -> crate::ApplyOutcome {
    let result = (|| -> Result<(), String> {
        let context = context()?;
        if context.read_state()?.is_none() {
            return Ok(());
        }
        let _lock = context.lock()?;
        let mut state = context.read_state()?.ok_or("missing enrollment")?;
        let actual = fs::metadata("/proc/self/exe").map_err(|e| e.to_string())?;
        boot_locked(
            context,
            &mut state,
            current_build,
            current_commit,
            (actual.dev(), actual.ino()),
        )
    })();
    match result {
        Ok(()) => crate::ApplyOutcome::NoUpdate,
        Err(error) => crate::ApplyOutcome::Deferred(error),
    }
}

fn boot_locked(
    context: &Context,
    state: &mut State,
    current_build: u64,
    current_commit: &str,
    actual_inode: (u64, u64),
) -> Result<(), String> {
    recover(context, state)?;
    let Some(trial) = state.trial.as_mut() else {
        return Ok(());
    };
    if trial.phase != Phase::Installed || trial.healthy {
        return Ok(());
    }
    let installed = checked_file(&context.target, BINARY_LIMIT, true)?
        .metadata()
        .map_err(|e| e.to_string())?;
    if actual_inode != (installed.dev(), installed.ino()) {
        return Ok(());
    }
    if trial.new.build != current_build || !crate::commit_matches(&trial.new.commit, current_commit)
    {
        state.outcome =
            "installed trial reports a different compiled identity; this startup is unhealthy"
                .into();
    }
    trial.starts = trial.starts.saturating_add(1);
    context.save(state)?;
    if state
        .trial
        .as_ref()
        .is_some_and(|trial| trial.starts > MAX_TRIAL_STARTS)
    {
        rollback_locked(context, state)?;
    }
    Ok(())
}

pub fn confirm(current_build: u64, current_commit: &str) -> bool {
    (|| -> Result<bool, String> {
        let Ok(context) = context() else {
            return Ok(true);
        };
        if context.read_state()?.is_none() {
            return Ok(true);
        }
        let _lock = context.lock()?;
        let mut state = context.read_state()?.ok_or("missing enrollment")?;
        // /proc/self/exe refers to this process's actual inode, not whichever
        // executable a sibling update most recently placed at the install path.
        let executable = File::open("/proc/self/exe").map_err(|e| e.to_string())?;
        let actual = executable.metadata().map_err(|e| e.to_string())?;
        confirm_locked(
            context,
            &mut state,
            current_build,
            current_commit,
            (actual.dev(), actual.ino()),
        )
    })()
    .unwrap_or(false)
}

fn confirm_locked(
    context: &Context,
    state: &mut State,
    current_build: u64,
    current_commit: &str,
    actual_inode: (u64, u64),
) -> Result<bool, String> {
    recover(context, state)?;
    let Some(trial) = state.trial.as_mut() else {
        return Ok(true);
    };
    if trial.phase != Phase::Installed
        || trial.new.build != current_build
        || !crate::commit_matches(&trial.new.commit, current_commit)
    {
        return Ok(true);
    }
    let installed = checked_file(&context.target, BINARY_LIMIT, true)?
        .metadata()
        .map_err(|e| e.to_string())?;
    if actual_inode != (installed.dev(), installed.ino()) {
        return Ok(false);
    }
    trial.healthy = true;
    context.save(state)?;
    Ok(true)
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

/// The head's authenticated policy is observed even when that release has no
/// native payload. Older payloads are judged under that SAME current roster,
/// never their historically attached (possibly now revoked) authorizations.
fn older_native_candidate(
    context: &Context,
    state: &mut State,
    head_tag: &str,
    head_proof: &Proof,
    candidates: &[aterm_update_core::release_catalog::ReleaseCandidate],
    pins: Pins<'_>,
    mut fetch: impl FnMut(
        &aterm_update_core::release_catalog::ReleaseCandidate,
        &Proof,
    ) -> Result<Proof, String>,
) -> Result<(String, Manifest, Proof), String> {
    let head_key = aterm_update_core::tag::parse_release_tag(head_tag)
        .map_err(|e| format!("invalid head: {e:?}"))?;
    for candidate in candidates {
        // Untrusted inventory is only a discovery hint, never authorization.
        // Mac-only history (including retired signers) cannot wedge a Linux
        // channel that offers no native payload in those unrelated releases.
        if !candidate.has_linux(native()?) {
            continue;
        }
        let tag = &candidate.tag;
        let key = aterm_update_core::tag::parse_release_tag(tag)
            .map_err(|e| format!("invalid catalog tag: {e:?}"))?;
        if key >= head_key {
            continue;
        }
        let proof = fetch(candidate, head_proof)?;
        let manifest = authenticate_tag(context, state, tag, &proof, pins)?;
        if manifest.linux_artifact(native()?)?.is_some() {
            return Ok((tag.clone(), manifest, proof));
        }
    }
    Err(format!(
        "no authenticated {} Linux executable is published in the release channel",
        native()?.triple()
    ))
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
            "installed {} on disk; awaiting a healthy new launch; existing sessions remain unchanged",
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
    let mut tags = None;
    let (tag, proof) = match download_proof(source, &head, None) {
        Ok(proof) => (head, proof),
        Err(error) if error.starts_with("missing appcast: ") => {
            let catalog = aterm_update_core::release_catalog::candidates(source, None)?;
            let tag = catalog
                .iter()
                .find(|candidate| candidate.current_appcast)
                .ok_or("no signed app release is published")?
                .tag
                .clone();
            let proof = download_proof(source, &tag, None)?;
            tags = Some(catalog);
            (tag, proof)
        }
        Err(error) => return Err(error),
    };
    let manifest = authenticate_tag(context, state, &tag, &proof, PRODUCTION_PINS)?;
    let target = native()?;
    let (tag, manifest, proof) = if manifest.linux_artifact(target)?.is_some() {
        (tag, manifest, proof)
    } else {
        let tags = match tags {
            Some(tags) => tags,
            None => aterm_update_core::release_catalog::candidates(source, None)?,
        };
        older_native_candidate(
            context,
            state,
            &tag,
            &proof,
            &tags,
            PRODUCTION_PINS,
            |candidate, roster| {
                download_named_proof(
                    source,
                    &candidate.tag,
                    &candidate.appcast_name(),
                    Some(roster),
                )
            },
        )?
    };
    let artifact = manifest
        .linux_artifact(target)?
        .ok_or("elected Linux artifact disappeared")?;
    if manifest.build_number < state.min_build {
        return Err("the published Linux build is below the signed minimum-build floor; no install permitted".into());
    }
    if manifest.build_number <= state.high_water || manifest.build_number <= state.rejected_build {
        state.outcome = format!(
            "no newer admissible Linux build (published {}, high-water {})",
            manifest.build_number, state.high_water
        );
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

fn held_stage(context: &Context, state: &mut State) -> Result<String, String> {
    let stage = state
        .staged
        .as_ref()
        .ok_or("missing verified Linux stage")?;
    state.outcome = format!(
        "verified Linux {} (build {}) staged; automatic application is held by current policy or changed/unavailable settings; run aterm update apply; installed build {} is unchanged",
        stage.version, stage.build, state.installed.build
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
        return held_stage(context, state);
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
        return held_stage(context, state);
    }
    result
}

pub fn check_now(current_build: u64, source: &Source) -> crate::UpdateStatus {
    let settings = crate::CheckSettings {
        source: source.clone(),
        auto_apply: true,
    };
    let provider: crate::CheckSettingsProvider =
        std::sync::Arc::new(move || Some(settings.clone()));
    check_with_settings(current_build, &provider, true)
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
    let attempt = (|| -> Result<(), String> {
        let settings = provider().ok_or("current update settings could not be read")?;
        let source = &settings.source;
        let context = context()?;
        let mut state = context.read_state()?.ok_or("Linux self-update is not enrolled; run aterm update enable explicitly for this installed copy")?;
        if !crate::enabled() || !state.enabled {
            return Err("Linux self-update enrollment is disabled".into());
        }
        let _lock = context.lock()?;
        state = context.read_state()?.ok_or("missing enrollment")?;
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
        result
    })();
    let mut status = status(current_build);
    if let Err(error) = attempt {
        status.outcome = error;
        status.failing_checks = status.failing_checks.max(1);
        status.failing_kind = "linux-update".into();
    }
    status
}

pub fn status(current_build: u64) -> crate::UpdateStatus {
    let mut output = crate::UpdateStatus::empty(crate::enabled(), current_build,
        "Linux self-update is not enrolled; run aterm update enable explicitly for this installed copy".into());
    match context().and_then(|context| context.read_state().map(|state| (context, state))) {
        Ok((context, Some(state))) => {
            let linux = linux_status(context, &state);
            let rejected_stage = state
                .staged
                .as_ref()
                .filter(|_| linux.staged_build.is_none())
                .map(|stage| stage.build);
            output.linux = Some(linux);
            output.enabled &= state.enabled;
            output.installable = true;
            output.outcome = state.outcome;
            if let Some(build) = rejected_stage {
                output.outcome.push_str(&format!("; recorded Linux stage {build} is unavailable or no longer admissible under the current build/revocation policy"));
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
                output.outcome = format!("automatic updates disabled; {}", output.outcome);
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

pub fn spawn_background_check(
    current_build: u64,
    source: crate::SourceProvider,
    notify: Option<crate::HealthNotify>,
) {
    spawn_background_check_with_settings(
        current_build,
        std::sync::Arc::new(move || {
            source().map(|source| crate::CheckSettings {
                source,
                auto_apply: true,
            })
        }),
        notify,
    )
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
            let mut notified = String::new();
            loop {
                let status = check_with_settings(current_build, &provider, false);
                if status.outcome != notified {
                    aterm_log::info!("aterm-update: {}", status.outcome);
                    if let Some(notify) = &notify {
                        notify("Linux software update".into(), status.outcome.clone());
                    }
                    notified = status.outcome;
                }
                std::thread::sleep(Duration::from_secs(60));
            }
        });
}

#[cfg(test)]
mod tests;
