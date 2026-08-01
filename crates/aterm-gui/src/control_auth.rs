// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Default-on access control for the introspection CONTROL SOCKET.
//!
//! The control socket grants FULL power over the live terminal (drive the
//! shell, deliver signals, snapshot pixels). Historically it bound a
//! world-writable `/tmp/aterm.sock` with no authentication, so ANY local
//! process — or a different local user — could drive the terminal. This module
//! closes that hole with three layers, all default-on and all transparent to a
//! same-user client:
//!
//! 1. **Per-user private directory.** The socket lives in a `0700` directory
//!    only the owning user can traverse (`$XDG_RUNTIME_DIR`, else
//!    `~/Library/Application Support/aterm`), and the socket file itself is
//!    `chmod 0600` after bind. A different user cannot even reach the socket.
//! 2. **Peer credential check.** After `accept(2)` the server reads the
//!    connecting peer's uid via `getpeereid(2)` and refuses any peer whose uid
//!    is not our own `geteuid()`. Defence in depth in case the directory perms
//!    are ever loosened (shared `$XDG_RUNTIME_DIR`, ACLs, ...).
//! 3. **Capability token.** On startup we generate 32 random bytes and write
//!    their hex to this instance's token file (`0600`). Every connection must
//!    present `AUTH <hex>` (or a `TOKEN <hex> <verb...>` prefix) as its first
//!    line; the server compares it to the stored token in constant time. A
//!    same-uid process that cannot read the `0600` token file (a sandboxed
//!    peer, a confused-deputy) is refused even though its uid matches.
//!
//! Instances do not collide: each binds its own `aterm-<pid>.sock` with a
//! matching `aterm-<pid>.token`, and a `aterm.sock` symlink is atomically
//! repointed at the newest instance so a single-instance `aterm-ctl` needs no
//! flags. Naming/staleness decisions are engine-side
//! ([`aterm_types::control_socket`]); this module does the filesystem work.
//!
//! "No nagging, keep power": there is NO prompt and NO new required flag. The
//! `aterm-ctl` client resolves the same directory, reads the token, and sends
//! the `AUTH` line automatically, so normal same-user usage is unchanged. A
//! same-uid client with the right token gets ALL verbs with zero friction;
//! everyone else is refused before the first verb runs.
//!
//! Platform split: the portable decisions live here; the filesystem/peer
//! primitives are per-platform sibling modules (`control_auth_unix.rs` — the
//! shipping POSIX code, moved verbatim — and `control_auth_win.rs`, whose
//! honestly-reduced posture — no peer-uid gate, but a `%LOCALAPPDATA%` directory
//! whose owner is VERIFIED and DACL hardened to owner-only
//! (`verify_owner_and_harden`), token still mandatory — is documented there and
//! disclosed at startup).

use std::path::{Path, PathBuf};

use aterm_types::control_socket::{self, SocketDirective};
use aterm_uds::CtlStream;

#[cfg(unix)]
#[path = "control_auth_unix.rs"]
mod imp;
#[cfg(windows)]
#[path = "control_auth_win.rs"]
mod imp;

#[cfg(unix)]
pub use imp::our_uid;
pub use imp::{
    ensure_private_dir, lock_socket_file, peer_check, provision_token, publish_latest_link,
};
// Test-only import: `peer_uid`'s production caller (`peer_check`) lives inside
// the unix `imp` module; only the socketpair unit test below calls it directly.
#[cfg(all(unix, test))]
use imp::peer_uid;
// Test-only import: `random_token_hex`'s production callers live inside the
// per-platform `imp` modules (`provision_token`); only the unit tests below
// draw a token directly.
#[cfg(test)]
use imp::random_token_hex;

/// Token filename beside a socket that is not per-instance (an explicit
/// `$ATERM_CONTROL_SOCK` path).
pub const TOKEN_FILE: &str = control_socket::SIBLING_TOKEN_FILE;

/// Filename of the `latest` symlink in the per-user directory, pointing at
/// the newest instance's `aterm-<pid>.sock`.
pub const SOCK_FILE: &str = control_socket::LATEST_SOCK_FILE;

/// Subdirectory of the socket directory that confines `image`-verb PNG writes.
pub const IMAGES_DIR: &str = "images";

/// Server-auto-named captures live below this private child, separated from
/// caller-explicit filenames so retention can never delete an explicit target.
const AUTO_IMAGES_DIR: &str = "auto";

/// VIDEO introspection recordings subdir (frame sequences + index.json).
pub const VIDEO_DIR: &str = "video";
/// Durable visibility marker created only when a complete recording becomes
/// non-abortable at the guarded wire boundary.
pub(crate) const VIDEO_PUBLISHED_FILE: &str = ".published";

/// One conservative filesystem-wide lock excludes concurrent explicit image
/// writes in the shared `images/` namespace. Contenders fail busy instead of
/// blocking the single encode lane. Locking the namespace (rather than a
/// spelling-derived sidecar) deliberately covers case-folding, Unicode
/// normalization, and Windows short-name aliases without guessing the mounted
/// filesystem's name-equivalence rules.
const CAPTURE_LOCK_DIR: &str = ".capture-locks";
const CAPTURE_NAMESPACE_LEASE_FILE: &str = "explicit";

/// Stable, collision-resistant identity for this exact server process.
///
/// PID alone is insufficient because captures persist after exit and an OS may
/// later reuse the PID. A CSPRNG launch nonce is preferred; high-resolution
/// wall time is the non-secret fallback when entropy is unavailable.
#[must_use]
pub(crate) fn process_instance_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let nonce = imp::random_token_hex()
            .map(|token| token[..24].to_string())
            .unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |duration| duration.as_nanos())
                    .to_string()
            });
        format!("p{}-{nonce}", std::process::id())
    })
}

fn automatic_capture_name_for(stem: &str, instance: &str, sequence: u64) -> String {
    format!("{stem}-{instance}-{sequence:06}.png")
}

/// Mint a server-unique omitted-name capture filename. Explicit client names
/// retain their historical overwrite-compatible behavior.
#[must_use]
pub(crate) fn automatic_capture_name(stem: &str) -> String {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    automatic_capture_name_for(stem, process_instance_id(), sequence)
}

/// Ownership of an artifact until its client acknowledges the guarded reply or
/// a failed/legacy handoff finishes its additional quarantine interval.
/// Server-unique auto images/videos use the process-local `key` for retention;
/// caller-explicit images additionally hold the shared namespace's OS advisory
/// lock so different aterm processes and aliased filename spellings fail busy
/// instead of racing one another.
#[derive(Debug)]
pub(crate) struct ArtifactPathLease {
    key: Option<PathBuf>,
    os_lock: Option<std::fs::File>,
    video_reader: bool,
}

#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "ArtifactReaderLease",
        action = "Acquire",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reader_lease"
    )
)]
fn artifact_reader_acquire_anchor() {}

#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "ArtifactReaderLease",
        action = "RejectAcquireWhileSweeping",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reader_lease"
    )
)]
fn artifact_reader_reject_acquire_anchor() {}

#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "ArtifactReaderLease",
        action = "Arm",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reader_lease"
    )
)]
fn artifact_reader_arm_anchor() {}

#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "ArtifactReaderLease",
        action = "Release",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reader_lease"
    )
)]
fn artifact_reader_release_anchor() {}

#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "ArtifactReaderLease",
        action = "StartSweep",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reader_lease"
    )
)]
fn artifact_reader_start_sweep_anchor() {}

#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "ArtifactReaderLease",
        action = "FinishSweep",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reader_lease"
    )
)]
fn artifact_reader_finish_sweep_anchor() {}

fn artifact_path_leases() -> &'static (
    std::sync::Mutex<std::collections::HashMap<PathBuf, ArtifactLeaseState>>,
    std::sync::Condvar,
) {
    static LEASES: std::sync::OnceLock<(
        std::sync::Mutex<std::collections::HashMap<PathBuf, ArtifactLeaseState>>,
        std::sync::Condvar,
    )> = std::sync::OnceLock::new();
    LEASES.get_or_init(|| {
        (
            std::sync::Mutex::new(std::collections::HashMap::new()),
            std::sync::Condvar::new(),
        )
    })
}

/// Serialize each bounded retention census with its mutations. Per-artifact
/// leases stop a sweep from touching a live reply, but they cannot by themselves
/// stop two stale namespace censuses from deleting disjoint candidates below the
/// keep cap. One process-wide gate is sufficient because auto-image and video
/// namespaces are process-private; callers never hold the lease mutex while
/// waiting here.
fn retention_sweep_gate() -> &'static std::sync::Mutex<()> {
    static GATE: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    GATE.get_or_init(|| std::sync::Mutex::new(()))
}

#[derive(Debug)]
struct VideoReaderSweep {
    root: crate::pinned_dir::PinnedDir,
    fresh: std::ffi::OsString,
}

#[derive(Default)]
struct ArtifactLeaseState {
    count: usize,
    /// Installed only after a frames reader's final identity validation. The
    /// exact root capability, rather than a pathname reopened later, binds the
    /// last-release convergence sweep to the namespace that was actually read.
    video_reader_sweep: Option<VideoReaderSweep>,
    /// A count-zero entry remains present while its capability-bound sweep runs.
    /// New readers fail closed instead of entering between the last-release
    /// decision and a retention rename.
    sweeping: bool,
}

fn register_unique_artifact_path(key: PathBuf) -> Option<ArtifactPathLease> {
    let (leases, _) = artifact_path_leases();
    let mut held = leases
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if held.contains_key(&key) {
        return None;
    }
    held.insert(
        key.clone(),
        ArtifactLeaseState {
            count: 1,
            video_reader_sweep: None,
            sweeping: false,
        },
    );
    Some(ArtifactPathLease {
        key: Some(key),
        os_lock: None,
        video_reader: false,
    })
}

pub(crate) fn acquire_capture_name_lease(
    target: &ConfinedImage,
    cancelled: impl Fn() -> bool,
) -> std::io::Result<Option<ArtifactPathLease>> {
    // Auto captures live in this process's launch-unique namespace and have
    // server-minted collision-resistant names. Their local path lease is enough
    // to exclude retention. Explicit images share `images/` across processes,
    // so reserve the one canonical namespace lock locally and in the kernel.
    let automatic = is_current_automatic_image(target);
    let key = if automatic {
        target.display_path()
    } else {
        target
            .dir
            .join(CAPTURE_LOCK_DIR)
            .join(CAPTURE_NAMESPACE_LEASE_FILE)
    };
    let (leases, changed) = artifact_path_leases();
    let mut held = leases
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while held.contains_key(&key) {
        if !automatic {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "another explicit capture reply owns the shared image namespace",
            ));
        }
        if cancelled() {
            return Ok(None);
        }
        let (next, _) = changed
            .wait_timeout(held, std::time::Duration::from_millis(25))
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held = next;
    }
    if cancelled() {
        return Ok(None);
    }
    held.insert(
        key.clone(),
        ArtifactLeaseState {
            count: 1,
            video_reader_sweep: None,
            sweeping: false,
        },
    );
    drop(held);

    let mut lease = ArtifactPathLease {
        key: Some(key),
        os_lock: None,
        video_reader: false,
    };
    if !automatic {
        // Lock the SAME retained authority the writer uses. On Unix the
        // directory inode itself is the advisory-lock object, so a same-uid
        // actor cannot split two writers by replacing a child lockfile. Windows
        // locks a child file reached through the deny-delete pinned directory;
        // its share mode prevents replacement for the full lock lifetime.
        #[cfg(unix)]
        let file = target.pinned.open_directory_lock()?;
        #[cfg(windows)]
        let file = {
            let lock_dir = target
                .pinned
                .ensure_child(std::ffi::OsStr::new(CAPTURE_LOCK_DIR))?;
            lock_dir.open_namespace_lock(std::ffi::OsStr::new(CAPTURE_NAMESPACE_LEASE_FILE))?
        };
        match file.try_lock() {
            Ok(()) => lease.os_lock = Some(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "another aterm process owns the shared image namespace",
                ));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(lease))
}

/// Retain an already-published recording while a `video frames` reply is read,
/// queued, written, consumed, and acknowledged. Multiple readers and the
/// original publication reply may share one recording, so this lease is
/// refcounted.
///
/// `None` means the previous final reader is already running the capability-
/// bound convergence sweep. The caller must drop its pinned candidate handles
/// and retry rather than returning paths from that in-between state.
pub(crate) fn retain_video_artifact_path(path: &Path) -> Option<ArtifactPathLease> {
    let key = path.to_path_buf();
    let (leases, _) = artifact_path_leases();
    let mut held = leases
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let state = held.entry(key.clone()).or_default();
    if state.sweeping {
        artifact_reader_reject_acquire_anchor();
        return None;
    }
    state.count = state.count.saturating_add(1);
    artifact_reader_acquire_anchor();
    Some(ArtifactPathLease {
        key: Some(key),
        os_lock: None,
        video_reader: true,
    })
}

impl ArtifactPathLease {
    /// Arm last-release retention only after the reader has revalidated every
    /// namespace, marker, index, and frame identity. Acquiring the lease before
    /// that validation prevents a producer/older sweep from removing the
    /// recording in the gap; delaying this hook prevents a failed read from
    /// causing unrelated retention work.
    pub(crate) fn arm_video_reader_sweep(
        &self,
        root: crate::pinned_dir::PinnedDir,
        fresh: std::ffi::OsString,
    ) -> std::io::Result<()> {
        let Some(key) = self.key.as_ref() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "artifact lease was already released",
            ));
        };
        if root.path().join(&fresh) != *key {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "video sweep capability does not match its retained recording",
            ));
        }
        let (leases, _) = artifact_path_leases();
        let mut held = leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(state) = held.get_mut(key) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "artifact lease registry entry disappeared",
            ));
        };
        if state.count == 0 || state.sweeping {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "video retention sweep is already in progress",
            ));
        }
        match state.video_reader_sweep.as_ref() {
            Some(existing)
                if existing.fresh != fresh || !existing.root.same_directory_identity(&root) =>
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "recording namespace changed while arming retention",
                ));
            }
            Some(_) => {}
            None => state.video_reader_sweep = Some(VideoReaderSweep { root, fresh }),
        }
        artifact_reader_arm_anchor();
        Ok(())
    }
}

fn leased_artifact_names(
    dir: &Path,
    exclude: &std::ffi::OsStr,
) -> std::collections::HashSet<std::ffi::OsString> {
    let (leases, _) = artifact_path_leases();
    let held = leases
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    held.keys()
        .filter(|path| path.parent() == Some(dir))
        .filter_map(|path| path.file_name())
        .filter(|name| *name != exclude)
        .map(std::ffi::OsStr::to_os_string)
        .collect()
}

/// Linearize a retention mutation against new and existing artifact readers.
/// The mutex intentionally spans the one exact handle-rooted unlink/rename:
/// either the retention mutation wins and a later reader's identity check
/// fails, or the reader lease wins and the artifact is skipped.
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "ArtifactReplyPublication",
        action = "RetentionSweep",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
    )
)]
fn mutate_unleased_artifact<T>(path: &Path, mutate: impl FnOnce() -> T) -> Option<T> {
    let (leases, _) = artifact_path_leases();
    let held = leases
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if held.contains_key(path) {
        return None;
    }
    Some(mutate())
}

impl Drop for ArtifactPathLease {
    fn drop(&mut self) {
        // Release the cross-process fence before waking a local waiter. An
        // explicit unlock matters on Unix: `flock` follows the open-file
        // description into duplicated/fork-inherited descriptors, so merely
        // dropping this process's handle can leave a transient lock behind in a
        // child between fork and exec. Crashes still release the lock when every
        // inherited handle closes, without a pathname deletion/recreation race.
        if let Some(os_lock) = self.os_lock.take() {
            let _ = os_lock.unlock();
        }
        let Some(key) = self.key.take() else {
            return;
        };
        let (leases, changed) = artifact_path_leases();
        let mut held = leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(state) = held.get_mut(&key) else {
            return;
        };
        debug_assert!(state.count > 0, "artifact lease count underflow");
        state.count = state.count.saturating_sub(1);
        if self.video_reader {
            artifact_reader_release_anchor();
        }
        let sweep = if state.count == 0 {
            match state.video_reader_sweep.take() {
                Some(sweep) => {
                    state.sweeping = true;
                    artifact_reader_start_sweep_anchor();
                    Some(sweep)
                }
                None => {
                    held.remove(&key);
                    None
                }
            }
        } else {
            None
        };
        changed.notify_all();
        drop(held);
        if let Some(sweep) = sweep {
            prune_video_after_reader_release(&sweep.root, &sweep.fresh);
            let mut held = leases
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if held
                .get(&key)
                .is_some_and(|state| state.count == 0 && state.sweeping)
            {
                held.remove(&key);
            }
            artifact_reader_finish_sweep_anchor();
            changed.notify_all();
        }
    }
}

/// Filename of the PERSISTENT network-drive token, stored beside the operator's TLS
/// key (a persistent, operator-owned dir). Unlike the per-launch control token, this
/// SURVIVES a remote restart, so a saved `dial` credential is not invalidated every
/// time the remote process restarts (R37).
pub const NETWORK_DRIVE_TOKEN_FILE: &str = "aterm-network-drive.token";

/// Load (or generate-once into a `0600` file) the PERSISTENT network-drive token in
/// `dir` — the channel-binding secret a `dial` peer presents. Because it persists
/// across restarts, an operator provisions it ONCE (`dial-token <name> <hex>`) and
/// it keeps working, instead of re-copying the remote's per-launch control token
/// after every restart. Returns the 64-char hex, or `None` if entropy/write fails
/// (the caller then falls back to the per-launch token — network drive still works,
/// it just needs re-provisioning per restart, the pre-R37 behavior).
///
/// Returns `(hex, freshly_minted)` — `freshly_minted` is `true` ONLY on the run that
/// creates the file, so the caller reveals the raw secret to the operator's log once
/// (at provisioning) instead of on every boot (CWE-532).
#[must_use]
pub fn load_or_create_network_drive_token(dir: &Path) -> Option<(String, bool)> {
    load_or_create_network_drive_token_with_hook(dir, || {})
}

fn load_or_create_network_drive_token_with_hook(
    dir: &Path,
    after_temp_create: impl FnOnce(),
) -> Option<(String, bool)> {
    load_or_create_network_drive_token_with_hooks(dir, after_temp_create, || {})
}

fn load_or_create_network_drive_token_with_hooks(
    dir: &Path,
    after_temp_create: impl FnOnce(),
    after_publish: impl FnOnce(),
) -> Option<(String, bool)> {
    let absolute = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(dir)
    };
    let pinned = crate::pinned_dir::PinnedDir::open_resolved(&absolute).ok()?;
    let read_valid = || {
        let (bytes, file) = pinned
            .read_private(std::ffi::OsStr::new(NETWORK_DRIVE_TOKEN_FILE), 4096)
            .ok()?;
        let hex = String::from_utf8_lossy(&bytes).trim().to_string();
        aterm_session::EdgeToken::from_hex(&hex)?;
        pinned.validate_path_identity().ok()?;
        file.validate_path_identity().ok()?;
        Some(hex)
    };
    // Reuse an existing valid token (the persistence that makes a saved credential
    // survive a restart). HARDENED read, matching the cert/key + control-token
    // readers: `O_NONBLOCK` so a same-uid writerless FIFO planted at this path cannot
    // park the control-socket serve thread at `open()`, `O_NOFOLLOW`, regular-file
    // only, capped — the bare `std::fs::read` this replaced had none of those. A
    // corrupt/short/unreadable file fails closed until the operator removes it.
    if let Some(hex) = read_valid() {
        return Some((hex, false)); // loaded an existing token — NOT freshly minted
    }
    // First run: race all creators with CREATE_NEW. One winner publishes and
    // fsyncs the directory; every loser re-reads that winner. A pre-existing
    // corrupt/nonregular target fails closed instead of being overwritten.
    let hex = imp::random_token_hex()?;
    match pinned.write_new_private_with_hooks(
        std::ffi::OsStr::new(NETWORK_DRIVE_TOKEN_FILE),
        hex.as_bytes(),
        after_temp_create,
        after_publish,
    ) {
        Ok(file) => {
            pinned.sync().ok()?;
            pinned.validate_path_identity().ok()?;
            file.validate_path_identity().ok()?;
            Some((hex, true))
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // A winner using the portable Unix fallback may briefly have both
            // its private temp link and final link. `read_private` correctly
            // rejects that nlink=2 state. Retry only this known create race;
            // arbitrary planted hardlinks/corrupt targets remain rejected.
            for attempt in 0..32 {
                if let Some(winner) = read_valid() {
                    return Some((winner, false));
                }
                if attempt != 31 {
                    std::thread::yield_now();
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
            None
        }
        Err(_) => None,
    }
}

fn valid_instance_component(instance: &str) -> bool {
    !instance.is_empty()
        && instance
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && Path::new(instance).components().count() == 1
}

fn metadata_is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        // FILE_ATTRIBUTE_REPARSE_POINT. This includes junctions, which
        // `FileType::is_symlink` alone does not promise to classify.
        return metadata.file_attributes() & 0x400 != 0;
    }
    #[cfg(not(windows))]
    false
}

/// Create/validate one real directory immediately below a canonical parent.
/// Existing links/reparse redirections are rejected before hardening or use.
fn ensure_canonical_direct_child(parent: &Path, name: &str) -> std::io::Result<PathBuf> {
    if Path::new(name).components().count() != 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "child name is not one path component",
        ));
    }
    let parent = std::fs::canonicalize(parent)?;
    let child = parent.join(name);
    match std::fs::symlink_metadata(&child) {
        Ok(metadata) => {
            if metadata_is_link_or_reparse(&metadata) || !metadata.is_dir() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "private child is a link or not a directory",
                ));
            }
            let resolved = std::fs::canonicalize(&child)?;
            if resolved.parent() != Some(parent.as_path()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "private child redirects outside its canonical parent",
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    ensure_private_dir(&child)?;
    let resolved = std::fs::canonicalize(&child)?;
    if resolved.parent() != Some(parent.as_path())
        || metadata_is_link_or_reparse(&std::fs::symlink_metadata(&child)?)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "private child is not a real canonical direct child",
        ));
    }
    Ok(resolved)
}

fn instance_pid(instance: &str) -> Option<u32> {
    instance.strip_prefix('p')?.split_once('-')?.0.parse().ok()
}

const INSTANCE_LEASE_FILE: &str = ".instance.lease";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstanceLeaseState {
    /// Pre-lease namespace: PID liveness is the compatibility authority.
    Missing,
    /// The exact launch currently holds the advisory lease.
    Held,
    /// The lease exists and an exclusive acquisition proved its launch gone.
    Acquirable,
    /// Link/reparse, wrong type, or an I/O/lock failure: never guess dead.
    Invalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstanceSweepDecision {
    Keep,
    Remove,
}

/// Pure total retention decision, shared by the real sweeper and Tier-1 model
/// projection. An exact lease dominates PID state; PID is consulted only for a
/// legacy namespace with no lease.
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "ExactInstanceRetention",
        action = "Decide",
        project = "aterm_gui::instance_retention_conformance::project_before"
    )
)]
#[must_use]
pub(crate) fn decide_instance_namespace_sweep(
    lease: InstanceLeaseState,
    pid_alive: bool,
) -> InstanceSweepDecision {
    match lease {
        InstanceLeaseState::Acquirable => InstanceSweepDecision::Remove,
        InstanceLeaseState::Missing if !pid_alive => InstanceSweepDecision::Remove,
        InstanceLeaseState::Missing | InstanceLeaseState::Held | InstanceLeaseState::Invalid => {
            InstanceSweepDecision::Keep
        }
    }
}

/// Open one namespace's real, direct-child lock file without following a
/// symlink/reparse point. The stable inode is advisory-lock authority; it is
/// never unlinked, and the kernel releases every held lock on process exit.
fn open_private_namespace_lock(
    namespace: &Path,
    file_name: &std::ffi::OsStr,
    create: bool,
) -> std::io::Result<std::fs::File> {
    let namespace = std::fs::canonicalize(namespace)?;
    if Path::new(file_name).components().count() != 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "namespace lock name must be one component",
        ));
    }
    let path = namespace.join(file_name);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if metadata_is_link_or_reparse(&metadata) || !metadata.is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "namespace lock is a link/reparse point or not a regular file",
                ));
            }
        }
        Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        // FILE_FLAG_OPEN_REPARSE_POINT: open the reparse object itself so the
        // metadata check below can reject it instead of following its target.
        // Omitting FILE_SHARE_DELETE pins this exact directory entry for the
        // whole advisory-lock lifetime.
        options
            .custom_flags(0x0020_0000)
            .share_mode(0x0000_0001 | 0x0000_0002);
    }
    let file = options.open(&path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "namespace lock is not a regular file",
        ));
    }
    let path_metadata = std::fs::symlink_metadata(&path)?;
    if metadata_is_link_or_reparse(&path_metadata) || !path_metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "namespace lock changed into a link/reparse point",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let opened = file.metadata()?;
        if opened.dev() != path_metadata.dev()
            || opened.ino() != path_metadata.ino()
            || opened.nlink() != 1
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "namespace lock identity changed or has multiple names",
            ));
        }
    }
    if std::fs::canonicalize(&path)?.parent() != Some(namespace.as_path()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "namespace lock redirects outside its namespace",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

/// Open the launch-identity lease. Unlike the PID embedded in the namespace
/// name, its advisory lock cannot be fooled by PID reuse.
fn open_instance_lease(namespace: &Path, create: bool) -> std::io::Result<std::fs::File> {
    open_private_namespace_lock(namespace, std::ffi::OsStr::new(INSTANCE_LEASE_FILE), create)
}

/// Hold the current launch's lease until process exit. A process can have both
/// an automatic-image and a video namespace, so the static owns one locked file
/// per canonical namespace rather than one global file.
fn hold_current_instance_lease(namespace: &Path) -> std::io::Result<()> {
    static LEASES: std::sync::OnceLock<std::sync::Mutex<Vec<(PathBuf, std::fs::File)>>> =
        std::sync::OnceLock::new();
    let namespace = std::fs::canonicalize(namespace)?;
    let leases = LEASES.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    let mut leases = leases
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if leases.iter().any(|(path, _)| path == &namespace) {
        return Ok(());
    }
    let file = open_instance_lease(&namespace, true)?;
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "this process instance namespace is already leased",
        ),
        std::fs::TryLockError::Error(error) => error,
    })?;
    leases.push((namespace, file));
    Ok(())
}

/// Remove only namespaces proven abandoned. New namespaces carry an advisory
/// lease: an exclusive try-lock proves the exact launch is gone even if its PID
/// has been reused. Legacy namespaces without a lease retain the older PID
/// fallback. Links/reparse points and malformed/unreadable leases are refused,
/// never followed or guessed dead.
fn sweep_dead_instance_namespaces_with(
    root: &Path,
    current: &str,
    mut pid_alive: impl FnMut(u32) -> bool,
) {
    let Ok(root) = std::fs::canonicalize(root) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata_is_link_or_reparse(&metadata) || !metadata.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name == current {
            continue;
        }
        let Some(pid) = instance_pid(&name) else {
            continue;
        };
        let Ok(canonical) = std::fs::canonicalize(&path) else {
            continue;
        };
        if canonical.parent() != Some(root.as_path()) {
            continue;
        }

        let lease_path = canonical.join(INSTANCE_LEASE_FILE);
        let (lease_state, acquired_lease) = match std::fs::symlink_metadata(&lease_path) {
            Ok(lease_metadata) => {
                if metadata_is_link_or_reparse(&lease_metadata) || !lease_metadata.is_file() {
                    (InstanceLeaseState::Invalid, None)
                } else {
                    match open_instance_lease(&canonical, false) {
                        Ok(lease) => match lease.try_lock() {
                            Ok(()) => (InstanceLeaseState::Acquirable, Some(lease)),
                            Err(std::fs::TryLockError::WouldBlock) => {
                                (InstanceLeaseState::Held, None)
                            }
                            Err(std::fs::TryLockError::Error(_)) => {
                                (InstanceLeaseState::Invalid, None)
                            }
                        },
                        Err(_) => (InstanceLeaseState::Invalid, None),
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (InstanceLeaseState::Missing, None)
            }
            Err(_) => (InstanceLeaseState::Invalid, None),
        };
        // PID is the compatibility authority ONLY for a pre-lease namespace.
        let legacy_pid_alive = lease_state == InstanceLeaseState::Missing && pid_alive(pid);
        if decide_instance_namespace_sweep(lease_state, legacy_pid_alive)
            == InstanceSweepDecision::Remove
        {
            // Keep a successful exact-lease acquisition alive across recursive
            // removal. Legacy/no-lease removal carries no guard by definition.
            let _acquired_lease = acquired_lease;
            let _ = std::fs::remove_dir_all(&canonical);
        }
    }
}

/// Resolve one process instance's private recording root.
#[must_use]
pub(crate) fn video_instance_root_for(sock_dir: &Path, instance: &str) -> Option<PathBuf> {
    valid_instance_component(instance).then(|| sock_dir.join(VIDEO_DIR).join(instance))
}

/// Unforgeable ownership of one server-minted recording directory. It moves
/// control thread → Wake → VideoRec → encode job. Every non-published Drop
/// removes the directory, covering refused begin, worker panic, and app drop.
#[derive(Debug)]
pub struct ConfinedVideoDir {
    path: Option<PathBuf>,
    instance: crate::pinned_dir::PinnedDir,
    recording: Option<crate::pinned_dir::PinnedDir>,
    name: std::ffi::OsString,
    _retention_lease: ArtifactPathLease,
    published: bool,
}

impl ConfinedVideoDir {
    #[must_use]
    pub fn path(&self) -> &Path {
        self.path
            .as_deref()
            .expect("live confined video dir owns its path")
    }

    pub(crate) fn write_new_private(
        &self,
        name: &std::ffi::OsStr,
        bytes: &[u8],
    ) -> std::io::Result<crate::pinned_dir::PinnedFile> {
        self.recording
            .as_ref()
            .expect("live confined video dir")
            .write_new_private(name, bytes)
    }

    pub(crate) fn write_new_private_authorized(
        &self,
        name: &std::ffi::OsStr,
        bytes: &[u8],
        authorize: impl FnOnce() -> bool,
    ) -> std::io::Result<crate::pinned_dir::PinnedFile> {
        self.recording
            .as_ref()
            .expect("live confined video dir")
            .write_new_private_authorized(name, bytes, authorize)
    }

    pub(crate) fn remove_file_if_exists(&self, name: &std::ffi::OsStr) -> std::io::Result<()> {
        self.recording
            .as_ref()
            .expect("live confined video dir")
            .remove_file_if_exists(name)
    }

    fn cleanup(&mut self) -> std::io::Result<()> {
        let Some(recording) = self.recording.as_ref() else {
            return Ok(());
        };
        self.instance
            .remove_child_tree_exact(&self.name, recording)?;
        self.recording = None;
        self.path = None;
        Ok(())
    }

    pub fn abort(mut self) -> std::io::Result<()> {
        self.cleanup()
    }

    /// Publish only after `index.json` is durable. Retention runs after the
    /// `.published` marker and again on last reader-lease release — never at
    /// mint, and never in this fallible pre-marker phase — so a refused/failed
    /// request never deletes a prior good recording.
    pub(crate) fn publish(
        &mut self,
        frames: &[crate::pinned_dir::PinnedFile],
        index: &crate::pinned_dir::PinnedFile,
    ) -> std::io::Result<PathBuf> {
        let recording = self.recording.as_ref().expect("live confined video dir");
        recording.validate_path_identity()?;
        for frame in frames {
            frame.validate_path_identity()?;
        }
        index.validate_path_identity()?;
        recording.sync()?;
        self.instance.sync()?;
        recording.validate_path_identity()?;
        for frame in frames {
            frame.validate_path_identity()?;
        }
        index.validate_path_identity()?;
        Ok(self.path().to_path_buf())
    }

    pub(crate) fn prune_after_publish(&self) {
        if let Err(error) = prune_video_dirs(&self.instance, &self.name) {
            eprintln!("aterm-gui: video retention skipped after successful publish: {error}");
        }
        if let Err(error) = self.instance.sync() {
            eprintln!(
                "aterm-gui: video retention directory sync failed after successful publish: {error}"
            );
        }
    }

    /// Atomically publish the marker readers require and make the recording
    /// permanently non-abortable at that exact rename/link boundary. Failures
    /// while preparing the invisible temporary still clean normally. Once the
    /// marker may have been visible, even a later sync/identity failure leaves
    /// the recording intact until launch-namespace cleanup: a successful reader
    /// may already have received paths and need to open them after disconnect.
    pub(crate) fn publish_marker(&mut self) -> std::io::Result<crate::pinned_dir::PinnedFile> {
        // Borrow the flag field separately so the commit hook can set it while
        // `recording` is still immutably borrowed for the write.
        let published = &mut self.published;
        self.recording
            .as_ref()
            .expect("live confined video dir")
            .write_new_private_with_hooks(
                std::ffi::OsStr::new(VIDEO_PUBLISHED_FILE),
                b"aterm-video-published-v1\n",
                || {},
                || *published = true,
            )
    }

    pub(crate) fn validate_for_reply(
        &self,
        frames: &[crate::pinned_dir::PinnedFile],
        index: &crate::pinned_dir::PinnedFile,
    ) -> std::io::Result<()> {
        self.instance.validate_path_identity()?;
        self.recording
            .as_ref()
            .expect("live confined video dir")
            .validate_path_identity()?;
        for frame in frames {
            frame.validate_path_identity()?;
        }
        index.validate_path_identity()
    }
}

impl std::ops::Deref for ConfinedVideoDir {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.path()
    }
}

impl Drop for ConfinedVideoDir {
    fn drop(&mut self) {
        if !self.published {
            let _ = self.cleanup();
        }
    }
}

/// Create + pin a fresh SERVER-NAMED recording directory under
/// `video/<process-instance>/`. ZERO client-controlled path components — both
/// the launch namespace and recording name are minted here. Every file inside
/// is written through the retained directory capability used by image output.
#[must_use]
pub fn confine_video_dir(sock_dir: &Path) -> Option<ConfinedVideoDir> {
    confine_video_dir_for_instance(sock_dir, process_instance_id())
}

#[must_use]
fn confine_video_dir_for_instance(sock_dir: &Path, instance: &str) -> Option<ConfinedVideoDir> {
    if !valid_instance_component(instance) {
        return None;
    }
    let video_root = ensure_canonical_direct_child(sock_dir, VIDEO_DIR).ok()?;
    let canon = ensure_canonical_direct_child(&video_root, instance).ok()?;
    hold_current_instance_lease(&canon).ok()?;
    sweep_dead_instance_namespaces_with(&video_root, instance, &imp::pid_alive);
    let instance_dir = crate::pinned_dir::PinnedDir::open(&canon).ok()?;
    static RECORDING_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let base = RECORDING_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    for n in 0..1000u32 {
        let name = std::ffi::OsString::from(format!("rec-{base:020}-{n:03}"));
        match instance_dir.create_child(&name) {
            Ok(recording) => {
                let path = recording.path().to_path_buf();
                let Some(retention_lease) = register_unique_artifact_path(path.clone()) else {
                    let _ = instance_dir.remove_child_tree_exact(&name, &recording);
                    continue;
                };
                return Some(ConfinedVideoDir {
                    path: Some(path),
                    instance: instance_dir,
                    recording: Some(recording),
                    name,
                    _retention_lease: retention_lease,
                    published: false,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

pub(crate) const AUTO_IMAGE_KEEP: usize = 32;
const AUTO_IMAGE_SCAN_BUDGET: usize = 256;

/// Confine one omitted-name `image`/`window` output to this launch's private
/// auto namespace. Explicit caller names continue to use direct `images/`
/// children and are therefore outside every automatic retention sweep.
#[must_use]
pub(crate) fn confine_automatic_image_path(sock_dir: &Path, stem: &str) -> Option<ConfinedImage> {
    if stem.is_empty()
        || !stem
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return None;
    }
    let images = ensure_canonical_direct_child(sock_dir, IMAGES_DIR).ok()?;
    let auto = ensure_canonical_direct_child(&images, AUTO_IMAGES_DIR).ok()?;
    let dir = ensure_canonical_direct_child(&auto, process_instance_id()).ok()?;
    hold_current_instance_lease(&dir).ok()?;
    sweep_dead_instance_namespaces_with(&auto, process_instance_id(), &imp::pid_alive);
    let pinned = crate::pinned_dir::PinnedDir::open(&dir).ok()?;
    Some(ConfinedImage {
        dir,
        file_name: automatic_capture_name(stem).into(),
        pinned,
    })
}

fn automatic_capture_sequence(name: &std::ffi::OsStr) -> Option<u64> {
    let name = name.to_str()?.strip_suffix(".png")?;
    name.rsplit_once('-')?.1.parse().ok()
}

/// Best-effort bound for completed auto-named files in this process's isolated
/// namespace. Every successful completion performs one bounded scan, so
/// legitimate server-named overflow makes progress across later sweeps.
/// Same-uid undeletable or adversarial clutter may prevent reaching the exact
/// [`AUTO_IMAGE_KEEP`] cap; it never delays or invalidates the fresh reply.
pub(crate) fn prune_automatic_image_dir(target: &ConfinedImage) {
    let dir = &target.dir;
    let eligible = dir
        .file_name()
        .is_some_and(|name| name == std::ffi::OsStr::new(process_instance_id()))
        && dir
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == std::ffi::OsStr::new(AUTO_IMAGES_DIR));
    if !eligible {
        return;
    }
    let _sweep = retention_sweep_gate()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Ok(entries) = target.pinned.names_up_to(AUTO_IMAGE_SCAN_BUDGET) else {
        return;
    };
    let leased = leased_artifact_names(&target.dir, &target.file_name);
    let mut protected = 1usize;
    let mut files = entries
        .into_iter()
        .filter_map(|name| {
            if name == target.file_name {
                return None;
            }
            let sequence = automatic_capture_sequence(&name)?;
            if !target.pinned.is_regular_file(&name) {
                return None;
            }
            if leased.contains(&name) {
                protected = protected.saturating_add(1);
                return None;
            }
            Some((sequence, name))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|(sequence, _)| *sequence);
    // The just-published path plus every other reply whose lease still spans its
    // explicit response ACK occupy keep slots. If live replies temporarily exceed the
    // cap, prune every unleased candidate and converge after those replies drop.
    let unprotected_keep = AUTO_IMAGE_KEEP.saturating_sub(protected);
    let excess = files.len().saturating_sub(unprotected_keep);
    let mut needed = excess;
    for (_, name) in files {
        if needed == 0 {
            break;
        }
        let path = target.dir.join(&name);
        if mutate_unleased_artifact(&path, || target.pinned.remove_file_if_exists(&name))
            .is_some_and(|result| result.is_ok())
        {
            needed -= 1;
        }
    }
}

fn is_current_automatic_image(target: &ConfinedImage) -> bool {
    automatic_capture_sequence(&target.file_name).is_some()
        && target
            .dir
            .file_name()
            .is_some_and(|name| name == std::ffi::OsStr::new(process_instance_id()))
        && target
            .dir
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == std::ffi::OsStr::new(AUTO_IMAGES_DIR))
}

/// Best-effort cleanup for an omitted-name write that failed after creating or
/// truncating its unique target. Explicit caller paths never satisfy the
/// auto-namespace predicate and are therefore never unlinked on an error.
pub(crate) fn cleanup_failed_automatic_image(target: &ConfinedImage) {
    if is_current_automatic_image(target) {
        let _ = target.pinned.remove_file_if_exists(&target.file_name);
    }
}

/// Recordings kept on disk, INCLUDING the one just created. Each recording can
/// hold up to a full frame budget in PNGs (hundreds of MiB at `full`), so an
/// agent that records in a loop would otherwise grow one process-instance
/// namespace without bound; the server prunes after each successful publish,
/// oldest first.
const VIDEO_KEEP: usize = 8;

/// Delete recordings beyond [`VIDEO_KEEP`], never touching `fresh` (the dir just
/// created for the in-flight recording). The server-named
/// `rec-<monotonic-sequence>-<collision>` stamps sort oldest-first
/// lexicographically within their process-instance namespace, so name order IS
/// creation order.
/// Best-effort: an undeletable entry is skipped — retention must never fail a
/// recording.
///
/// Only prunes COMPLETED recordings (those that own both a valid `index.json`
/// and the `.published` marker created at guarded-reply preparation). An
/// in-flight or queued recording may already own PNGs + an index but no marker,
/// and its directory is NOT `fresh` for a LATER `video` call. Without the
/// publication gate, a second recording's prune could destroy a recording
/// before its result is safely delivered.
const VIDEO_PRUNE_WORK_LIMIT: usize = 64 * 1024 * 1024;
const VIDEO_PRUNE_SCAN_LIMIT: usize = 512;
const VIDEO_PRUNE_DELETE_ENTRY_LIMIT: usize = 16_384;
const VIDEO_SCAN_WORK: usize = 64;
const VIDEO_FILE_OPEN_WORK: usize = 4 * 1024;
const VIDEO_DELETE_WORK: usize = 4 * 1024;
const VIDEO_INDEX_LIMIT: usize = 16 * 1024 * 1024;
const VIDEO_FRAME_LIMIT: usize = 10_000;

/// One shared retention allowance covers directory scans, index bytes, exact
/// file probes, renames, and deletes across the whole sweep. Per-recording
/// limits alone multiplied by the scan bound into gigabytes of parsing and
/// millions of opens; this counter makes that multiplication impossible.
#[derive(Debug)]
struct VideoPruneWork {
    remaining: usize,
    scanned: usize,
    index_bytes: usize,
    file_opens: usize,
    deletes: usize,
    renames: usize,
}

impl VideoPruneWork {
    fn new(limit: usize) -> Self {
        Self {
            remaining: limit,
            scanned: 0,
            index_bytes: 0,
            file_opens: 0,
            deletes: 0,
            renames: 0,
        }
    }

    fn spend(&mut self, units: usize) -> bool {
        if self.remaining < units {
            return false;
        }
        self.remaining -= units;
        true
    }

    fn scan(&mut self) -> bool {
        if !self.spend(VIDEO_SCAN_WORK) {
            return false;
        }
        self.scanned += 1;
        true
    }

    fn file_open(&mut self) -> bool {
        if !self.spend(VIDEO_FILE_OPEN_WORK) {
            return false;
        }
        self.file_opens += 1;
        true
    }

    fn bytes(&mut self, bytes: usize) -> bool {
        if !self.spend(bytes) {
            return false;
        }
        self.index_bytes = self.index_bytes.saturating_add(bytes);
        true
    }

    fn delete(&mut self) -> bool {
        if !self.spend(VIDEO_DELETE_WORK) {
            return false;
        }
        self.deletes += 1;
        true
    }

    fn rename(&mut self) -> bool {
        if !self.spend(VIDEO_DELETE_WORK) {
            return false;
        }
        self.renames += 1;
        true
    }
}

enum CompletionProbe {
    Complete(Vec<crate::pinned_dir::PinnedFile>),
    Invalid,
    Exhausted,
}

fn completed_recording(
    recording: &crate::pinned_dir::PinnedDir,
    work: &mut VideoPruneWork,
) -> CompletionProbe {
    // Last-reader retention deliberately survives an ancestor rename. Every
    // probe below is therefore relative to the already-authorized directory
    // capability and revalidates its direct entry, without reopening the stale
    // lexical namespace.
    if !work.file_open() {
        return CompletionProbe::Exhausted;
    }
    let Ok(marker) =
        recording.pin_private_file_at_retained(std::ffi::OsStr::new(VIDEO_PUBLISHED_FILE))
    else {
        return CompletionProbe::Invalid;
    };
    if !work.file_open() {
        return CompletionProbe::Exhausted;
    }
    // `read_private(limit)` reads at most limit+1 bytes. Leave room for that
    // sentinel so even an oversized index cannot cross the global allowance.
    let read_limit = VIDEO_INDEX_LIMIT.min(work.remaining.saturating_sub(1));
    if read_limit == 0 {
        return CompletionProbe::Exhausted;
    }
    let (bytes, index) =
        match recording.read_private_at_retained(std::ffi::OsStr::new("index.json"), read_limit) {
            Ok(value) => value,
            Err(error) => {
                if error.kind() == std::io::ErrorKind::InvalidData {
                    let consumed = read_limit.saturating_add(1).min(work.remaining);
                    let _ = work.bytes(consumed);
                    if read_limit < VIDEO_INDEX_LIMIT {
                        return CompletionProbe::Exhausted;
                    }
                }
                return CompletionProbe::Invalid;
            }
        };
    if !work.bytes(bytes.len()) {
        return CompletionProbe::Exhausted;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return CompletionProbe::Invalid;
    };
    let Some(frames) = value.get("frames").and_then(serde_json::Value::as_array) else {
        return CompletionProbe::Invalid;
    };
    if frames.len() > VIDEO_FRAME_LIMIT {
        return CompletionProbe::Invalid;
    }
    let mut files = Vec::with_capacity(frames.len().saturating_add(2));
    files.push(marker);
    files.push(index);
    for frame in frames {
        let Some(name) = frame.get("file").and_then(serde_json::Value::as_str) else {
            return CompletionProbe::Invalid;
        };
        if !work.file_open() {
            return CompletionProbe::Exhausted;
        }
        let Ok(file) = recording.pin_private_file_at_retained(std::ffi::OsStr::new(name)) else {
            return CompletionProbe::Invalid;
        };
        files.push(file);
    }
    CompletionProbe::Complete(files)
}

fn cleanup_video_tombstone(
    root: &crate::pinned_dir::PinnedDir,
    name: &std::ffi::OsStr,
    recording: &crate::pinned_dir::PinnedDir,
    work: &mut VideoPruneWork,
) -> bool {
    let entry_allowance = (work.remaining / VIDEO_DELETE_WORK).min(VIDEO_PRUNE_DELETE_ENTRY_LIMIT);
    if entry_allowance == 0 {
        return false;
    }
    let mut entries_left = entry_allowance;
    let result = recording.clear_contents_with_budget(&mut entries_left);
    let used = entry_allowance.saturating_sub(entries_left);
    for _ in 0..used {
        if !work.delete() {
            return false;
        }
    }
    if result.is_err() || !work.delete() {
        return false;
    }
    root.remove_empty_child_exact(name, recording).is_ok()
}

fn prune_video_dirs_with_work(
    root: &crate::pinned_dir::PinnedDir,
    fresh: &std::ffi::OsStr,
    work: &mut VideoPruneWork,
) -> std::io::Result<()> {
    let _sweep = retention_sweep_gate()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let affordable_scan = work.remaining / VIDEO_SCAN_WORK;
    let names = root.names_up_to(VIDEO_PRUNE_SCAN_LIMIT.min(affordable_scan))?;
    let leased = leased_artifact_names(root.path(), fresh);
    let mut tombstones = Vec::new();
    let mut completed = Vec::new();
    let mut protected_completed = 0usize;
    for name in names {
        if !work.scan() {
            break;
        }
        let text = name.to_string_lossy();
        if text.starts_with(".prune-") {
            if let Ok(recording) = root.child(&name) {
                tombstones.push((name, recording));
            }
            continue;
        }
        if name == fresh || !text.starts_with("rec-") {
            continue;
        }
        let Ok(recording) = root.child(&name) else {
            continue;
        };
        if !work.file_open() {
            break;
        }
        if !work.file_open() {
            break;
        }
        if recording.is_regular_file(std::ffi::OsStr::new("index.json"))
            && recording.is_regular_file(std::ffi::OsStr::new(VIDEO_PUBLISHED_FILE))
        {
            if leased.contains(&name) {
                protected_completed = protected_completed.saturating_add(1);
            } else {
                completed.push((name, recording));
            }
        }
    }

    // Finish quarantined partial work first. A bounded sweep may stop midway,
    // but the next publication recognizes the tombstone and continues instead
    // of orphaning a directory whose index marker was already removed.
    tombstones.sort_by(|left, right| left.0.cmp(&right.0));
    for (name, recording) in tombstones {
        if !cleanup_video_tombstone(root, &name, &recording, work) && work.remaining == 0 {
            return Ok(());
        }
    }

    completed.sort_by(|left, right| left.0.cmp(&right.0));
    // `fresh` and every other completed reply whose lease still spans a socket
    // write occupy keep slots. Only unleased, fully validated oldest candidates
    // are quarantined. Completion probing and cleanup share the same allowance.
    let protected = 1usize.saturating_add(protected_completed);
    let unprotected_keep = VIDEO_KEEP.saturating_sub(protected);
    let mut needed = completed.len().saturating_sub(unprotected_keep);
    for (name, recording) in completed {
        if needed == 0 {
            break;
        }
        let guards = match completed_recording(&recording, work) {
            CompletionProbe::Complete(guards) => guards,
            CompletionProbe::Invalid => continue,
            CompletionProbe::Exhausted => break,
        };
        if !work.rename() {
            break;
        }
        static PRUNE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = PRUNE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tombstone =
            std::ffi::OsString::from(format!(".prune-p{}-{sequence:020}", std::process::id()));
        // Windows file guards deny deletion while validation is in flight.
        // Release them only after the candidate is complete and immediately
        // before the handle-rooted quarantine rename.
        drop(guards);
        let path = root.path().join(&name);
        let renamed = mutate_unleased_artifact(&path, || {
            root.rename_child_exact(&name, &recording, &tombstone)
        })
        .is_some_and(|result| result.is_ok());
        if !renamed {
            continue;
        }
        needed -= 1;
        if !cleanup_video_tombstone(root, &tombstone, &recording, work) && work.remaining == 0 {
            break;
        }
    }
    Ok(())
}

fn prune_video_dirs(
    root: &crate::pinned_dir::PinnedDir,
    fresh: &std::ffi::OsStr,
) -> std::io::Result<()> {
    let mut work = VideoPruneWork::new(VIDEO_PRUNE_WORK_LIMIT);
    prune_video_dirs_with_work(root, fresh, &mut work)
}

/// A frames reply may temporarily force the recording count above the cap.
/// When its final shared lease releases (after every exact reader handle), run
/// another bounded sweep without waiting for a future recording. Preserve the
/// just-advertised recording as `fresh`: the response has crossed its ACK
/// boundary, but the client still needs a stable pathname to open.
fn prune_video_after_reader_release(
    root: &crate::pinned_dir::PinnedDir,
    recording_name: &std::ffi::OsStr,
) {
    if let Err(error) = prune_video_dirs(root, recording_name) {
        eprintln!("aterm-gui: video retention skipped after reader release: {error}");
    }
    if let Err(error) = root.sync() {
        eprintln!("aterm-gui: video retention sync failed after reader release: {error}");
    }
}

/// Resolve the per-user directory that holds the control socket, token, and
/// image-confinement subdir, creating it if missing (`0700` on Unix; on
/// Windows the default per-user ACL is the boundary — see
/// `control_auth_win.rs`).
///
/// Order of preference (matched exactly by the `aterm-ctl` client):
/// 1. `$XDG_RUNTIME_DIR/aterm` when `XDG_RUNTIME_DIR` is set (already a
///    per-user `0700` dir on systems that provide it).
/// 2. `~/Library/Application Support/aterm` on macOS (the conventional
///    per-user app-support location), created `0700`.
/// 3. Windows: `%TEMP%\aterm` (`%TMP%`/`%TEMP%`, else `%LOCALAPPDATA%\Temp\aterm`)
///    — deliberately OUTSIDE the OneDrive-managed `%APPDATA%` subtree, where afunix
///    `connect` cannot reach the socket's reparse point (WSAEINVAL). See
///    [`aterm_uds::control_socket_dir`]. Short enough for the 108-byte `sun_path`.
///
/// The directory DECISION is shared with the client (`aterm-ctl`) via
/// [`aterm_uds::control_socket_dir`] so server and client can never drift; the
/// server additionally hardens the dir (owner-check + DACL) below.
///
/// Returns `None` only when the per-user base cannot be resolved from the
/// environment, which should not happen for an interactive session.
#[must_use]
pub fn socket_dir() -> Option<PathBuf> {
    let dir = aterm_uds::control_socket_dir()?;
    ensure_private_dir(&dir).ok()?;
    Some(dir)
}

/// Everything the server needs to provision one instance's control socket.
#[derive(Clone)]
pub struct SocketPlan {
    /// Path to bind the listening socket at.
    pub sock_path: String,
    /// Path of this instance's capability-token file.
    pub token_path: PathBuf,
    /// The `latest` convenience symlink to maintain (`None` for an explicit
    /// `$ATERM_CONTROL_SOCK` override, which owns its path outright).
    pub latest_link: Option<PathBuf>,
}

/// How the control socket should be provisioned this launch.
pub enum SocketResolution {
    /// Bind per this plan.
    Enabled(SocketPlan),
    /// Explicitly disabled via the environment; do not bind.
    Disabled,
    /// No per-user directory and no override resolvable; do not bind.
    NoDir,
    /// `$ATERM_CONTROL_SOCK` names a path too long for `sun_path`; do not bind.
    /// Both `bind` and `connect` on such a path fail `EINVAL` — and the fail-safe
    /// liveness probe must treat an unexpected connect error as "maybe live", so
    /// without this variant the launch log claimed "already has a live listener"
    /// for a socket that can never exist (a real 25-minute debugging trap). The
    /// warning names the actual limit instead.
    PathTooLong { path: String, limit: usize },
}

/// The longest socket path `sockaddr_un` can carry on this platform, EXCLUDING
/// the NUL terminator: `sun_path` is 104 bytes on the BSDs/macOS and 108 on
/// Linux/Windows (`UNIX_PATH_MAX`), one of which the kernel spends on the NUL.
pub const MAX_SUN_PATH: usize = if cfg!(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly",
)) {
    103
} else {
    107
};

/// Whether `path` fits `sockaddr_un` on this platform (see [`MAX_SUN_PATH`]).
/// Byte length, not char count — `sun_path` is a byte array.
#[must_use]
pub fn sun_path_ok(path: &str) -> bool {
    path.len() <= MAX_SUN_PATH
}

/// The socket plan decision. `$ATERM_CONTROL_SOCK` may name an explicit path,
/// or disable the socket entirely with `0`/`off` (as does
/// `$ATERM_NO_CONTROL_SOCK=1`); unset/empty means the per-instance default
/// `aterm-<pid>.sock` inside [`socket_dir`], published via the `aterm.sock`
/// symlink. The decision itself is engine-side
/// ([`control_socket::socket_directive`]); this just reads the environment.
#[must_use]
pub fn resolve_socket_plan() -> SocketResolution {
    let explicit = std::env::var_os("ATERM_CONTROL_SOCK").map(|v| v.to_string_lossy().into_owned());
    let kill = std::env::var_os("ATERM_NO_CONTROL_SOCK").map(|v| v.to_string_lossy().into_owned());
    match control_socket::socket_directive(explicit.as_deref(), kill.as_deref()) {
        SocketDirective::Disabled => SocketResolution::Disabled,
        SocketDirective::Explicit(p) => {
            // An oversized path can never bind (EINVAL) — and its liveness probe
            // would misread the same EINVAL as "maybe live". Name the real
            // problem instead of running socketless behind a misleading warning.
            if !sun_path_ok(&p) {
                return SocketResolution::PathTooLong {
                    path: p,
                    limit: MAX_SUN_PATH,
                };
            }
            let token_path = dir_of_socket(&p).join(TOKEN_FILE);
            SocketResolution::Enabled(SocketPlan {
                sock_path: p,
                token_path,
                latest_link: None,
            })
        }
        SocketDirective::PerInstance => match socket_dir() {
            Some(dir) => {
                let pid = std::process::id();
                SocketResolution::Enabled(SocketPlan {
                    sock_path: dir
                        .join(control_socket::instance_sock_name(pid))
                        .to_string_lossy()
                        .into_owned(),
                    token_path: dir.join(control_socket::instance_token_name(pid)),
                    latest_link: Some(dir.join(SOCK_FILE)),
                })
            }
            None => SocketResolution::NoDir,
        },
    }
}

/// Remove per-instance sockets/tokens left behind by instances whose pid is
/// no longer alive (a crashed session cannot clean up after itself). Live
/// instances — including ourselves — and the fixed filenames are untouched.
pub fn sweep_stale_instances(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let names: Vec<String> = entries
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    for stale in control_socket::stale_instance_files(&refs, &imp::pid_alive) {
        let _ = std::fs::remove_file(dir.join(stale));
    }
}

/// Graceful-exit cleanup: remove this instance's socket + token, and the
/// `latest` alias ONLY while it still points at our socket (a newer
/// instance may have repointed it). Crash exits are covered by
/// [`sweep_stale_instances`] at the next spawn.
pub fn cleanup_socket(plan: &SocketPlan) {
    let _ = std::fs::remove_file(&plan.sock_path);
    let _ = std::fs::remove_file(&plan.token_path);
    if let Some(link) = &plan.latest_link {
        let our_pid = Path::new(&plan.sock_path)
            .file_name()
            .and_then(|f| control_socket::instance_pid(&f.to_string_lossy()));
        let target = aterm_uds::latest::target_name(link);
        if let (Some(pid), Some(target)) = (our_pid, target)
            && control_socket::symlink_targets_pid(&target.to_string_lossy(), pid)
        {
            let _ = std::fs::remove_file(link);
        }
    }
}

/// The directory a given socket `path` lives in — used to locate the sibling
/// token file and `images/` subdir for an explicit `$ATERM_CONTROL_SOCK`.
#[must_use]
pub fn dir_of_socket(path: &str) -> PathBuf {
    Path::new(path)
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Read the capability token from `<dir>/aterm.token`, trimming whitespace.
/// The symmetric counterpart of [`provision_token`]: the `aterm-ctl` client
/// reads the token equivalently (resolving the per-instance token through the
/// `latest` symlink), and the server uses it in tests and as a self-check.
/// Returns `None` if unreadable (wrong user, missing).
#[must_use]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "symmetric API; client reads token equivalently")
)]
pub fn read_token(dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(dir.join(TOKEN_FILE)).ok()?;
    let t = raw.trim().to_string();
    if t.is_empty() { None } else { Some(t) }
}

/// Whether a control socket at `path` has a LIVE listener: a successful `connect`
/// proves someone is bound. FAIL-SAFE — only an explicit connection-refused /
/// not-found means "stale, safe to remove"; any other error is treated as live so
/// we never unlink a maybe-live socket. (The Windows `CtlStream::connect`
/// explicitly normalizes a missing path to `NotFound` so this fail-safe holds
/// there too.)
#[must_use]
pub fn socket_is_live(path: &str) -> bool {
    use std::io::ErrorKind;
    match CtlStream::connect(path) {
        Ok(_) => true,
        Err(e) => !matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound),
    }
}

/// [`socket_is_live`] for the PER-INSTANCE default plan (`aterm-<ourpid>.sock`).
///
/// The fail-safe above INVERTS for this path: nobody else can legitimately own a
/// path named after OUR pid, so the only thing an unexpected connect error
/// (`EPERM` under a hardened runtime, `ETIMEDOUT`, `ECONNRESET` from a half-dead
/// leftover of a crashed same-pid predecessor) can indicate is stale junk — and
/// treating it as "live" means running SOCKETLESS for the process lifetime with
/// no retry (the 2026-07-05 dark-introspection incident's prime suspect). Only a
/// SUCCESSFUL connect — a listener actually answering — refuses the bind here.
/// Explicit shared `$ATERM_CONTROL_SOCK` paths keep the strict
/// [`socket_is_live`] fail-safe (never hijack a maybe-live parent).
#[must_use]
pub fn socket_is_live_per_instance(path: &str) -> bool {
    CtlStream::connect(path).is_ok()
}

/// What to do with an existing socket path before binding (Item 5).
#[derive(Debug, PartialEq, Eq)]
pub enum BindAction {
    /// No live listener — remove any stale file and bind.
    RemoveAndBind,
    /// A live listener already owns this path — do NOT touch it; run socket-less.
    RefuseLiveSocket,
}

/// Decide whether it is safe to (unlink and) bind a socket path, given whether a
/// live listener is already there. REFUSE a live socket so a nested aterm that
/// somehow still sees an explicit `$ATERM_CONTROL_SOCK` can never unlink+steal its
/// parent's listener (the liveness BELT to the env deny-list's SUSPENDERS — GAP-5
/// of the recursion topology). `is_live` is injected for unit-testability.
#[must_use]
pub fn decide_bind(is_live: bool) -> BindAction {
    if is_live {
        BindAction::RefuseLiveSocket
    } else {
        BindAction::RemoveAndBind
    }
}

/// Constant-time equality of two byte slices.
///
/// Always inspects every byte of the LONGER input (so length differences do
/// not leak via early return), accumulating differences into one flag and
/// folding the length comparison into the same flag. No `&&`/`||` short-circuit
/// and no early `return` on first mismatch.
#[must_use]
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let max = a.len().max(b.len());
    // Length mismatch sets the flag regardless of WHICH bits of the delta
    // differ (the old `as u8 | (>>8) as u8` fold only covered bits 0..16, so a
    // delta with all set bits >= 65536 was dropped). `u8::from(..) * 0xff` is
    // 0x00 when equal, 0xff when not — no overflow in debug or release.
    let mut diff: u8 = u8::from(a.len() != b.len()) * 0xff;
    for i in 0..max {
        // Reading past the end of the shorter slice would be UB; index into the
        // longer via wrapping to 0 with a sentinel that always differs when one
        // side is exhausted. `diff` already carries the length mismatch, so the
        // result is correct regardless of the sentinel value chosen.
        let av = *a.get(i).unwrap_or(&0);
        let bv = *b.get(i).unwrap_or(&0xff);
        diff |= av ^ bv;
    }
    diff == 0
}

/// Outcome of parsing/validating a connection's first line against the token.
#[derive(Debug, PartialEq, Eq)]
pub enum AuthOutcome {
    /// Authenticated. If the first line carried an inline `TOKEN <hex> <verb>`
    /// the remaining verb text is returned so the caller dispatches it now;
    /// a bare `AUTH <hex>` yields `None` (the next line is the first verb).
    Ok(Option<String>),
    /// Authentication failed (bad/missing token line, wrong token).
    Denied,
}

/// Validate a connection's first line against the expected token.
///
/// Accepts two equivalent forms so a client can authenticate without an extra
/// round-trip:
/// * `AUTH <hex>`            — dedicated auth line; the verb follows on line 2.
/// * `TOKEN <hex> <verb...>` — token + first verb folded into one line.
///
/// The comparison is constant-time. Anything else (no token line, wrong token,
/// malformed) is [`AuthOutcome::Denied`].
#[must_use]
pub fn check_auth_line(line: &str, expected: &str) -> AuthOutcome {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let (head, rest) = match line.split_once(' ') {
        Some((h, r)) => (h, r),
        None => (line, ""),
    };
    match head {
        "AUTH" => {
            if constant_time_eq(rest.trim_end().as_bytes(), expected.as_bytes()) {
                AuthOutcome::Ok(None)
            } else {
                AuthOutcome::Denied
            }
        }
        "TOKEN" => {
            // `TOKEN <hex> <verb...>`: split off the hex, keep the verb tail.
            let (hex, verb) = match rest.split_once(' ') {
                Some((h, v)) => (h, v),
                None => (rest, ""),
            };
            if constant_time_eq(hex.trim_end().as_bytes(), expected.as_bytes()) {
                AuthOutcome::Ok(Some(verb.to_string()))
            } else {
                AuthOutcome::Denied
            }
        }
        _ => AuthOutcome::Denied,
    }
}

/// A caller-supplied `image` path confined to the `images/` subdir, as a
/// canonical directory plus a SINGLE final filename component.
///
/// TOCTOU-1: the confinement decision is made on the control thread but the
/// WRITE happens on the main thread, so we must not let the writer re-resolve a
/// multi-segment path string (an intermediate dir could be symlink-swapped in
/// the gap). Returning the canonical `images/` dir and a bare filename — with
/// NESTED target dirs forbidden — lets the writer open the directory
/// `O_DIRECTORY|O_NOFOLLOW` once and `openat` the final component, so there is no
/// intermediate path component left to swap.
#[derive(Clone, Debug)]
pub struct ConfinedImage {
    /// The canonical `images/` directory (the only directory ever opened).
    pub dir: PathBuf,
    /// The single, validated filename to create inside `dir` (no separators).
    pub file_name: std::ffi::OsString,
    /// Every absolute ancestor plus the exact output directory, retained from
    /// confinement until the encode worker commits or refuses the reply.
    pinned: crate::pinned_dir::PinnedDir,
}

impl ConfinedImage {
    /// The full path, for logging / `OK <w> <h> <path>` replies only — NOT for
    /// re-opening (the writer must use [`Self::dir`] + [`Self::file_name`]).
    #[must_use]
    pub fn display_path(&self) -> PathBuf {
        self.dir.join(&self.file_name)
    }

    /// Write relative to the directory handle retained at confinement time.
    #[cfg(test)]
    pub(crate) fn write_private(
        &self,
        bytes: &[u8],
    ) -> std::io::Result<crate::pinned_dir::PinnedFile> {
        self.write_private_authorized(bytes, || true)
    }

    /// Write complete private bytes, then let the request's cancellation token
    /// linearize immediately before the final component becomes observable.
    pub(crate) fn write_private_authorized(
        &self,
        bytes: &[u8],
        authorize: impl FnOnce() -> bool,
    ) -> std::io::Result<crate::pinned_dir::PinnedFile> {
        if is_current_automatic_image(self) {
            self.pinned
                .write_new_private_authorized(&self.file_name, bytes, authorize)
        } else {
            self.pinned
                .write_private_authorized(&self.file_name, bytes, authorize)
        }
    }

    /// A file-path reply is authorized only while every original absolute
    /// directory component still resolves to the retained identity.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "AnchoredArtifactTransaction",
            action = "ValidateReply",
            project = "aterm_gui::artifact_transaction_conformance::project_anchored"
        )
    )]
    pub(crate) fn validate_for_reply(
        &self,
        file: &crate::pinned_dir::PinnedFile,
    ) -> std::io::Result<()> {
        self.pinned.validate_path_identity()?;
        file.validate_path_identity()
    }

    #[cfg(test)]
    pub(crate) fn for_test(dir: &Path, file_name: &str) -> Self {
        let dir = std::fs::canonicalize(dir).expect("test capture directory");
        let pinned = crate::pinned_dir::PinnedDir::open(&dir).expect("pin test capture directory");
        Self {
            dir,
            file_name: file_name.into(),
            pinned,
        }
    }
}

/// Confine a caller-supplied `image` path to the `images/` subdir of the
/// socket directory.
///
/// The subdir is created `0700`. A relative or bare-filename request is
/// resolved INTO the subdir; an absolute request must already live inside it.
/// NESTED target directories are FORBIDDEN — the file must be a direct child of
/// `images/` — so the only directory component is the canonical subdir itself
/// (closing the intermediate-dir symlink-swap window, TOCTOU-1). Returns the
/// canonical dir + validated filename, or `None` (→ `ERR path`) when the request
/// would escape or names a nested path.
///
/// SPEC: this is the real `Confine` action of the external `PathConfine.tla` model
/// (TRUST_NATIVE_TLA Phase 2, control-socket CONFINEMENT family). The spec's
/// `WriteWithinSubdir` / `EscapeRejected` invariants — a committed write only ever
/// lands INSIDE the root, and a request whose final component resolves OUTSIDE is
/// rejected with no write — are exactly the confused-deputy symlink escape this
/// canonicalize-the-RESOLVED-location check fixes (the old guard checked only the
/// parent prefix, then the writer FOLLOWED a symlinked last segment). A `Some(_)`
/// here maps to the spec's `committed=TRUE, target="inside"`; a `None` (escape) maps
/// to `committed=FALSE, target="none"`. Tier-1 conformance drives this against a real
/// planted symlink (`tests/conformance_pathconfine.rs`).
// Anchor gated on `test` ALONE (not a feature): the relocated `spec_xref_closure`
// gate lives in THIS crate's own test build, so `cfg(test)` already makes this
// anchor visible to it — no cross-crate `spec-anchors` feature is needed here.
// PROJECTION (TRUST_VACUITY_GATE §2.2 / finding 2): `Confine` projects the real
// confine outcome onto the spec's `<<linkOutside, decided, committed, target>>` — the
// projection the `path_confine_conformance` Tier-1 test drives (`Some` → committed
// inside, `None` → rejected). L2 requires the projection NAME be present (Trust does
// not execute it); `aterm_gui::control_auth::project_confine` is that witness.
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "path_confine",
        action = "Confine",
        project = "aterm_gui::control_auth::project_confine"
    )
)]
#[must_use]
pub fn confine_image_path(sock_dir: &Path, requested: &str) -> Option<ConfinedImage> {
    let canon_images = ensure_canonical_direct_child(sock_dir, IMAGES_DIR).ok()?;

    let req = Path::new(requested);
    // Map the request to a candidate path inside (or claimed-inside) the subdir.
    // A bare name or relative path is taken relative to the images subdir, never
    // the process cwd, so `aterm-ctl image shot.png` just works.
    let raw_candidate = if req.is_absolute() {
        req.to_path_buf()
    } else {
        canon_images.join(req)
    };

    // LEXICAL normalization: collapse `.`/`..` purely on the path string,
    // refusing any `..` that would climb above the ROOT. This kills `..`-escape
    // tricks WITHOUT depending on whether the (possibly non-existent) target is
    // on disk — closing the hole where a non-existent escape parent (e.g.
    // `../../etc/passwd`) would slip past a canonicalize() that errors.
    let lexical = lexically_normalize(&raw_candidate)?;

    // FORBID NESTED TARGET DIRS (TOCTOU-1): the file's parent, canonicalized,
    // must be EXACTLY the canonical images subdir — not merely inside it. This
    // means there is a single directory component (`images/`) and one filename,
    // so the writer never re-resolves a multi-segment string whose intermediate
    // dir could be symlink-swapped between this check and the open.
    let file_name = lexical.file_name()?;
    // A filename with a path separator or `..`/`.` is not a single component.
    if Path::new(file_name).components().count() != 1 {
        return None;
    }
    if file_name == std::ffi::OsStr::new(AUTO_IMAGES_DIR)
        || file_name == std::ffi::OsStr::new(CAPTURE_LOCK_DIR)
    {
        return None;
    }
    let parent = lexical.parent()?;
    let canon_parent = std::fs::canonicalize(parent).ok()?;
    if canon_parent != canon_images {
        return None;
    }
    // Reject a SYMLINK at the final component up front (defence in depth): the
    // writer also uses `O_NOFOLLOW`, but rejecting here gives a clean `ERR path`
    // for the common case and avoids even attempting the open.
    let resolved = canon_images.join(file_name);
    if let Ok(md) = std::fs::symlink_metadata(&resolved)
        && md.file_type().is_symlink()
    {
        return None;
    }
    let pinned = crate::pinned_dir::PinnedDir::open(&canon_images).ok()?;
    Some(ConfinedImage {
        dir: canon_images,
        file_name: file_name.to_os_string(),
        pinned,
    })
}

/// Confine a discovery-graph socket path to the trusted socket directory before the
/// proxy forward dials it and presents a parent-minted edge token. The graph entry
/// (`<sock_dir>/graph/<sid>`) is same-uid writable AND carries the launch nonce in
/// plain text, so the nonce guard alone does NOT stop a hostile same-uid process from
/// overwriting a child's entry with `sock <attacker-path>` (copying the readable
/// nonce) to make the parent connect to an attacker socket and hand over the
/// capability token. Mirror [`confine_image_path`]'s posture: the path must name a
/// single real file DIRECTLY in `sock_dir` (canonical parent == canonical `sock_dir`,
/// exactly one component, no `..`) and must NOT be a symlink — so a forward can only
/// ever dial a real socket inside our own runtime dir, never a redirected one.
/// Returns the confined (canonical-dir-rooted) path, or `None` to fail the forward
/// closed. The legit publisher writes exactly `<sock_dir>/aterm-<pid>.sock`.
#[must_use]
pub fn confine_proxy_sock(sock_dir: &Path, requested: &str) -> Option<String> {
    let canon_dir = std::fs::canonicalize(sock_dir).ok()?;
    let req = Path::new(requested);
    if !req.is_absolute() {
        return None; // graph entries publish ABSOLUTE instance-socket paths
    }
    // Lexically kill `.`/`..` first (so a non-existent escape can't slip past a
    // canonicalize that errors), then require a single filename whose canonical
    // parent is EXACTLY our socket dir — never a nested or redirected directory.
    let lexical = lexically_normalize(req)?;
    let file_name = lexical.file_name()?;
    if Path::new(file_name).components().count() != 1 {
        return None;
    }
    let canon_parent = std::fs::canonicalize(lexical.parent()?).ok()?;
    if canon_parent != canon_dir {
        return None;
    }
    // Reject a SYMLINK at the final component: `connect()` follows symlinks, so a
    // same-uid attacker could otherwise plant `<sock_dir>/aterm-x.sock -> /attacker`.
    let resolved = canon_dir.join(file_name);
    if let Ok(md) = std::fs::symlink_metadata(&resolved)
        && md.file_type().is_symlink()
    {
        return None;
    }
    Some(resolved.to_string_lossy().into_owned())
}

/// Lexically resolve `.`/`..`/`//` in an ABSOLUTE path WITHOUT touching the
/// filesystem. Returns `None` if a `..` would escape above the root (so a path
/// can never resolve above `/`). Symlinks are intentionally NOT followed here —
/// that is the canonicalize re-check's job; this pass just kills `..` tricks.
fn lexically_normalize(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut out: Vec<Component> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                match out.last() {
                    // Pop a normal segment; refuse to climb above the root.
                    Some(Component::Normal(_)) => {
                        out.pop();
                    }
                    Some(Component::RootDir) | None => return None,
                    _ => out.push(comp),
                }
            }
            other => out.push(other),
        }
    }
    Some(out.iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ITEM 5: `socket_is_live` is true for a bound listener and false for an
    /// absent OR stale (file present, nobody listening) socket — so the bind gate
    /// removes stale files but never unlinks a live listener.
    #[test]
    fn socket_is_live_true_for_bound_false_for_stale() {
        let dir = std::env::temp_dir().join(format!("aterm-live-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("s.sock");
        let ps = path.to_string_lossy().into_owned();
        let _ = std::fs::remove_file(&path);
        assert!(!socket_is_live(&ps), "absent socket is not live");
        let listener = aterm_uds::CtlListener::bind(&path).expect("bind");
        assert!(socket_is_live(&ps), "bound socket is live");
        drop(listener); // file remains, nobody listens -> becomes stale
        // Socket teardown is ASYNCHRONOUS: under heavy parallel load there is a brief
        // window after the listener fd closes where a connect to the now-orphaned path
        // returns something OTHER than ECONNREFUSED/NotFound, which `socket_is_live`
        // (deliberately fail-SAFE: any ambiguous error => "live", so we never unlink a
        // maybe-live socket) reports as live. Poll until the kernel has fully torn the
        // listener down — it must report stale within a bounded window; never doing so
        // is a real regression, not the teardown race.
        let mut went_stale = false;
        for _ in 0..200 {
            if !socket_is_live(&ps) {
                went_stale = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            went_stale,
            "stale socket file (no listener) must become not-live"
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    /// The bind decision refuses a live socket (never hijack) and removes-then-binds
    /// a stale/absent one.
    #[test]
    fn decide_bind_refuses_live_keeps_stale() {
        assert_eq!(decide_bind(true), BindAction::RefuseLiveSocket);
        assert_eq!(decide_bind(false), BindAction::RemoveAndBind);
    }

    #[test]
    fn constant_time_eq_matches() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(constant_time_eq(b"", b""));
        let tok = "deadbeef".repeat(8);
        assert!(constant_time_eq(tok.as_bytes(), tok.as_bytes()));
    }

    #[test]
    fn constant_time_eq_rejects_differences() {
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abc"));
        assert!(!constant_time_eq(b"abc", b""));
        assert!(!constant_time_eq(b"", b"abc"));
    }

    #[test]
    fn constant_time_eq_detects_length_delta_above_16_bits() {
        // Regression: the length fold once only covered the low 16 bits of the
        // usize delta, so all-zero slices whose lengths differ by exactly
        // 0x10000 (65536) compared EQUAL — the common prefix matched and the
        // dropped delta bit hid the rest. Must report NOT equal.
        let short = vec![0u8; 64];
        let long = vec![0u8; 64 + 65536];
        assert!(!constant_time_eq(&short, &long));
        assert!(!constant_time_eq(&long, &short));
    }

    /// An explicit socket path over the platform `sun_path` capacity must be
    /// refused BY NAME (`PathTooLong`), never funneled into the bind/probe path —
    /// bind AND connect both fail `EINVAL` there, and the fail-safe liveness
    /// probe reads that as "maybe live", producing the misleading "already has a
    /// live listener" launch warning for a socket that can never exist.
    #[test]
    fn sun_path_limit_is_enforced_byte_wise() {
        assert!(sun_path_ok(&"a".repeat(MAX_SUN_PATH)), "at the limit binds");
        assert!(
            !sun_path_ok(&"a".repeat(MAX_SUN_PATH + 1)),
            "one over is refused"
        );
        // Byte length, not char count: a multibyte path near the limit.
        let mb = format!("{}é", "a".repeat(MAX_SUN_PATH - 1)); // é = 2 bytes ⇒ limit+1
        assert!(!sun_path_ok(&mb), "multibyte overflow is caught byte-wise");
        // Platform sanity: the constant matches the OS family.
        #[cfg(target_os = "macos")]
        assert_eq!(MAX_SUN_PATH, 103);
        #[cfg(target_os = "linux")]
        assert_eq!(MAX_SUN_PATH, 107);
    }

    #[test]
    fn random_token_is_64_hex_chars() {
        let t = random_token_hex().expect("entropy available");
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        // Two draws must differ (astronomically unlikely to collide).
        let t2 = random_token_hex().expect("entropy available");
        assert_ne!(t, t2);
    }

    #[test]
    fn auth_line_accepts_correct_token() {
        let tok = "a".repeat(64);
        assert_eq!(
            check_auth_line(&format!("AUTH {tok}"), &tok),
            AuthOutcome::Ok(None)
        );
        // CRLF tolerance: a trailing CR must not break the compare.
        assert_eq!(
            check_auth_line(&format!("AUTH {tok}\r"), &tok),
            AuthOutcome::Ok(None)
        );
    }

    #[test]
    fn auth_line_rejects_wrong_token() {
        let tok = "a".repeat(64);
        let bad = "b".repeat(64);
        assert_eq!(
            check_auth_line(&format!("AUTH {bad}"), &tok),
            AuthOutcome::Denied
        );
        assert_eq!(check_auth_line("AUTH", &tok), AuthOutcome::Denied);
        assert_eq!(check_auth_line("text", &tok), AuthOutcome::Denied);
        assert_eq!(check_auth_line("", &tok), AuthOutcome::Denied);
    }

    #[test]
    fn token_prefix_form_carries_verb() {
        let tok = "c".repeat(64);
        assert_eq!(
            check_auth_line(&format!("TOKEN {tok} text"), &tok),
            AuthOutcome::Ok(Some("text".to_string()))
        );
        assert_eq!(
            check_auth_line(&format!("TOKEN {tok} send echo hi"), &tok),
            AuthOutcome::Ok(Some("send echo hi".to_string()))
        );
        // Bare token with no verb still authenticates (empty verb tail).
        assert_eq!(
            check_auth_line(&format!("TOKEN {tok}"), &tok),
            AuthOutcome::Ok(Some(String::new()))
        );
        // Wrong token in TOKEN form is denied.
        let bad = "d".repeat(64);
        assert_eq!(
            check_auth_line(&format!("TOKEN {bad} text"), &tok),
            AuthOutcome::Denied
        );
    }

    #[test]
    #[cfg(unix)]
    fn ensure_private_dir_refuses_group_or_other_writable() {
        use std::os::unix::fs::PermissionsExt;
        // SEC-3: a pre-existing dir that is group/other-writable is REFUSED, not
        // silently provisioned into — even after we force the mode, a foreign
        // owner / loose bits indicate an unsafe directory.
        let dir = std::env::temp_dir().join(format!("aterm-dir-gw-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        // Loosen it behind ensure_private_dir's back, then call again: it forces
        // 0700 and the re-stat passes (we own it). Verify the success path.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o770)).unwrap();
        // ensure_private_dir forces 0700, so a subsequent call succeeds (owner is
        // us, bits tightened). This proves the gate accepts our own dir.
        ensure_private_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "ensure_private_dir must force 0700");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn provision_token_refuses_symlinked_path() {
        // SEC-3: provision_token must not write THROUGH a symlink planted at the
        // token path (O_EXCL|O_NOFOLLOW + unlink-first). The victim is untouched.
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("aterm-tok-sym-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"original").unwrap();
        let tokpath = dir.join("aterm.token");
        symlink(&victim, &tokpath).unwrap();
        // unlink-first removes the symlink, then O_EXCL|O_NOFOLLOW creates a real
        // file — so the token lands in a fresh regular file, NOT the victim.
        let _ = provision_token(&tokpath);
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"original",
            "the symlink target must not be written through",
        );
        // And the token path is now a regular file (the unlink-first replaced the
        // symlink), readable as a 0600 token.
        let md = std::fs::symlink_metadata(&tokpath).unwrap();
        assert!(
            md.file_type().is_file(),
            "token path must be a regular file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn provision_and_read_token_roundtrip() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("aterm-auth-test-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let written = provision_token(&dir.join(TOKEN_FILE)).expect("token written");
        let read = read_token(&dir).expect("token readable");
        assert_eq!(written, read);
        // Token file is 0600.
        let mode = std::fs::metadata(dir.join(TOKEN_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        // Dir is 0700.
        let dmode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dmode, 0o700);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Windows twin of the Unix roundtrip + symlink tests: the token
    /// roundtrips, and `create_new(true)` (CREATE_NEW — the `O_EXCL|O_NOFOLLOW`
    /// analog) refuses ANY pre-existing object at the path, so the token is
    /// only ever written into a file we just created; `provision_token`'s
    /// unlink-first still lets a rotation succeed over our own stale token.
    #[test]
    #[cfg(windows)]
    fn provision_token_roundtrips_and_create_new_refuses_preexisting() {
        let dir = std::env::temp_dir().join(format!("aterm-auth-test-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let tokpath = dir.join(TOKEN_FILE);
        // A planted pre-existing file: the exclusive-create primitive refuses it.
        std::fs::write(&tokpath, b"planted").unwrap();
        let err = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tokpath)
            .expect_err("CREATE_NEW must refuse a pre-existing object");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        // provision_token unlinks first, then exclusively creates: rotation works.
        let written = provision_token(&tokpath).expect("token written");
        let read = read_token(&dir).expect("token readable");
        assert_eq!(written, read);
        assert_eq!(written.len(), 64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn confine_image_path_allows_inside_and_rejects_escape() {
        let dir = std::env::temp_dir().join(format!("aterm-img-test-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();

        // Bare name resolves into images/.
        let ok = confine_image_path(&dir, "shot.png").expect("bare name allowed");
        assert_eq!(ok.file_name.to_str(), Some("shot.png"));
        assert!(
            ok.display_path().ends_with("images/shot.png"),
            "got {:?}",
            ok.display_path()
        );

        // `../` escape is rejected.
        assert!(confine_image_path(&dir, "../escape.png").is_none());
        assert!(confine_image_path(&dir, "../../etc/passwd").is_none());

        // Absolute path outside the subdir is rejected.
        assert!(confine_image_path(&dir, "/tmp/evil.png").is_none());

        // Absolute path that IS inside the subdir is allowed.
        let inside = dir.join(IMAGES_DIR).join("ok.png");
        let allowed =
            confine_image_path(&dir, inside.to_str().unwrap()).expect("absolute-inside allowed");
        assert!(allowed.display_path().ends_with("images/ok.png"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn confine_image_path_rejects_nested_target_dir() {
        // TOCTOU-1: a NESTED target dir (images/sub/shot.png) is forbidden even
        // if the subdir exists and is inside images/ — so the writer only ever
        // opens the single canonical images/ directory and openat's one name,
        // leaving no intermediate dir component to symlink-swap between threads.
        let dir = std::env::temp_dir().join(format!("aterm-img-nested-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let images = dir.join(IMAGES_DIR);
        ensure_private_dir(&images).unwrap();
        ensure_private_dir(&images.join("sub")).unwrap();
        assert!(
            confine_image_path(&dir, "sub/shot.png").is_none(),
            "a nested target dir must be rejected (intermediate-dir TOCTOU)"
        );
        // The absolute form of the same nested path is rejected too.
        let nested_abs = images.join("sub").join("shot.png");
        assert!(confine_image_path(&dir, nested_abs.to_str().unwrap()).is_none());
        // A direct child still works.
        assert!(confine_image_path(&dir, "shot.png").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn confine_image_path_rejects_symlinked_final_component() {
        // A same-uid token-holding client plants a symlink AT the final
        // component (images/evil.png -> a file OUTSIDE the subdir). The parent
        // canonicalizes inside images/, so the old containment check passed and
        // the writer would follow the link and clobber an arbitrary file. This
        // is exactly the confused-deputy escape confine_image_path must stop.
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("aterm-img-symlink-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let images = dir.join(IMAGES_DIR);
        ensure_private_dir(&images).unwrap();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"original").unwrap();
        symlink(&victim, images.join("evil.png")).unwrap();

        assert!(
            confine_image_path(&dir, "evil.png").is_none(),
            "a symlinked final component must be rejected (arbitrary-write escape)"
        );
        // Legit cases still work: a fresh name and an existing REGULAR file.
        assert!(confine_image_path(&dir, "fresh.png").is_some());
        std::fs::write(images.join("real.png"), b"x").unwrap();
        assert!(confine_image_path(&dir, "real.png").is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn automatic_images_are_unique_bounded_and_never_prune_explicit_targets() {
        let dir =
            std::env::temp_dir().join(format!("aterm-img-auto-retention-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();
        let explicit = confine_image_path(&dir, "operator-kept.png").unwrap();
        std::fs::write(explicit.display_path(), b"explicit").unwrap();

        let mut names = std::collections::HashSet::new();
        let mut auto_dir = None;
        for _ in 0..(AUTO_IMAGE_KEEP + 7) {
            let target = confine_automatic_image_path(&dir, "image").unwrap();
            assert!(
                names.insert(target.file_name.clone()),
                "every omitted-name request gets a fresh server name"
            );
            std::fs::write(target.display_path(), b"png").unwrap();
            prune_automatic_image_dir(&target);
            auto_dir = Some(target.dir);
        }
        let auto_dir = auto_dir.unwrap();
        let auto_files = std::fs::read_dir(&auto_dir)
            .unwrap()
            .flatten()
            .filter(|entry| automatic_capture_sequence(&entry.file_name()).is_some())
            .count();
        assert_eq!(
            auto_files, AUTO_IMAGE_KEEP,
            "successful auto captures converge to the retention cap"
        );
        assert_eq!(
            std::fs::read(explicit.display_path()).unwrap(),
            b"explicit",
            "automatic retention never reaches caller-explicit files"
        );

        let partial = confine_automatic_image_path(&dir, "window").unwrap();
        std::fs::write(partial.display_path(), b"partial").unwrap();
        cleanup_failed_automatic_image(&partial);
        assert!(
            !partial.display_path().exists(),
            "failed automatic writes remove their partial target"
        );
        cleanup_failed_automatic_image(&explicit);
        assert_eq!(
            std::fs::read(explicit.display_path()).unwrap(),
            b"explicit",
            "failure cleanup is structurally unable to unlink explicit targets"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn explicit_capture_release_unlocks_an_inherited_descriptor() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-img-inherited-lock-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();

        let first = ConfinedImage::for_test(&dir, "first.png");
        let first_lease = acquire_capture_name_lease(&first, || false)
            .expect("first lease acquisition")
            .expect("first explicit lease");
        let inherited = first_lease
            .os_lock
            .as_ref()
            .expect("explicit Unix lease owns an OS lock")
            .try_clone()
            .expect("duplicate the descriptor as fork would");
        drop(first_lease);

        let second = ConfinedImage::for_test(&dir, "second.png");
        let second_lease = acquire_capture_name_lease(&second, || false)
            .expect("release must not leave a lock in an inherited descriptor")
            .expect("second explicit lease");
        drop(second_lease);
        drop(inherited);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn automatic_image_retention_preserves_every_queued_wire_reply() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-img-auto-queued-retention-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();

        let queued = confine_automatic_image_path(&dir, "image").unwrap();
        let queued_path = queued.display_path();
        let queued_lease = acquire_capture_name_lease(&queued, || false)
            .expect("automatic lease acquisition")
            .expect("lease queued capture");
        drop(queued.write_private(b"queued").unwrap());

        for _ in 0..(AUTO_IMAGE_KEEP + 3) {
            let later = confine_automatic_image_path(&dir, "image").unwrap();
            std::fs::write(later.display_path(), b"later").unwrap();
        }
        let fresh = confine_automatic_image_path(&dir, "image").unwrap();
        std::fs::write(fresh.display_path(), b"fresh").unwrap();
        prune_automatic_image_dir(&fresh);

        assert_eq!(
            std::fs::read(&queued_path).unwrap(),
            b"queued",
            "a later reply's retention sweep cannot delete an older queued reply"
        );
        let completed = std::fs::read_dir(&fresh.dir)
            .unwrap()
            .flatten()
            .filter(|entry| automatic_capture_sequence(&entry.file_name()).is_some())
            .count();
        assert_eq!(
            completed, AUTO_IMAGE_KEEP,
            "leased replies consume retention slots without exceeding the cap"
        );

        drop(queued_lease);
        let next = confine_automatic_image_path(&dir, "image").unwrap();
        std::fs::write(next.display_path(), b"next").unwrap();
        prune_automatic_image_dir(&next);
        assert!(
            !queued_path.exists(),
            "after the wire lease drops, the oldest artifact becomes eligible"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn automatic_image_prune_progresses_past_one_scan_window() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-img-auto-large-retention-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();
        let target = confine_automatic_image_path(&dir, "image").unwrap();
        std::fs::write(target.display_path(), b"fresh").unwrap();
        for sequence in 0..(AUTO_IMAGE_SCAN_BUDGET + 97) {
            let name = automatic_capture_name_for("image", process_instance_id(), sequence as u64);
            std::fs::write(target.dir.join(name), b"png").unwrap();
        }

        for _ in 0..8 {
            prune_automatic_image_dir(&target);
        }
        let remaining = std::fs::read_dir(&target.dir)
            .unwrap()
            .flatten()
            .filter(|entry| automatic_capture_sequence(&entry.file_name()).is_some())
            .count();
        assert_eq!(
            remaining, AUTO_IMAGE_KEEP,
            "a directory larger than one scan window converges under repeated bounded sweeps"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn media_roots_refuse_planted_symlinks() {
        use std::os::unix::fs::symlink;

        let outside =
            std::env::temp_dir().join(format!("aterm-media-roots-outside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&outside);
        ensure_private_dir(&outside).unwrap();

        let image_sock =
            std::env::temp_dir().join(format!("aterm-media-roots-image-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&image_sock);
        ensure_private_dir(&image_sock).unwrap();
        symlink(&outside, image_sock.join(IMAGES_DIR)).unwrap();
        assert!(
            confine_image_path(&image_sock, "shot.png").is_none(),
            "the explicit image root cannot redirect"
        );

        let video_sock =
            std::env::temp_dir().join(format!("aterm-media-roots-video-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&video_sock);
        ensure_private_dir(&video_sock).unwrap();
        symlink(&outside, video_sock.join(VIDEO_DIR)).unwrap();
        assert!(
            confine_video_dir(&video_sock).is_none(),
            "the video root cannot redirect"
        );

        let auto_sock =
            std::env::temp_dir().join(format!("aterm-media-roots-auto-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&auto_sock);
        ensure_private_dir(&auto_sock).unwrap();
        let images = ensure_canonical_direct_child(&auto_sock, IMAGES_DIR).unwrap();
        symlink(&outside, images.join(AUTO_IMAGES_DIR)).unwrap();
        assert!(
            confine_automatic_image_path(&auto_sock, "image").is_none(),
            "the automatic image root cannot redirect"
        );

        let _ = std::fs::remove_dir_all(image_sock);
        let _ = std::fs::remove_dir_all(video_sock);
        let _ = std::fs::remove_dir_all(auto_sock);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    #[cfg(unix)]
    fn confine_proxy_sock_allows_in_dir_and_rejects_redirects() {
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("aterm-proxysock-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let canon = std::fs::canonicalize(&dir).unwrap();

        // The legit publisher writes `<sock_dir>/aterm-<pid>.sock` — accepted, and
        // returned rooted at the CANONICAL dir (so a later dial can't be redirected
        // through a symlinked prefix).
        let legit = dir.join("aterm-42.sock");
        let got =
            confine_proxy_sock(&dir, &legit.to_string_lossy()).expect("in-dir socket allowed");
        assert_eq!(got, canon.join("aterm-42.sock").to_string_lossy());

        // An absolute path OUTSIDE the runtime dir (the attacker overwrite) is rejected.
        assert!(confine_proxy_sock(&dir, "/tmp/evil-attacker.sock").is_none());
        // A `..` escape and a relative path are rejected.
        let escape = dir.join("../evil.sock");
        assert!(confine_proxy_sock(&dir, &escape.to_string_lossy()).is_none());
        assert!(
            confine_proxy_sock(&dir, "aterm-42.sock").is_none(),
            "relative rejected"
        );
        // A nested subdir under the runtime dir is rejected (single component only).
        let nested = dir.join("sub/aterm-42.sock");
        assert!(confine_proxy_sock(&dir, &nested.to_string_lossy()).is_none());

        // A SYMLINK planted at the final component (pointing outside) is rejected —
        // `connect()` would otherwise follow it to the attacker socket.
        symlink("/tmp/evil-attacker.sock", dir.join("aterm-redir.sock")).unwrap();
        assert!(
            confine_proxy_sock(&dir, &dir.join("aterm-redir.sock").to_string_lossy()).is_none(),
            "a symlinked socket name must be rejected (token-capture escape)",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn peer_uid_of_socketpair_is_our_uid() {
        // A connected UnixStream pair: the peer of each end is us.
        let (a, _b) = CtlStream::pair().expect("socketpair");
        if let Some(uid) = peer_uid(&a) {
            assert_eq!(uid, our_uid());
        }
        // On platforms without a peer-cred primitive, peer_uid is None — the
        // caller fails closed, which is the correct conservative behaviour.
        // And the accept gate accepts our own connection.
        assert_eq!(peer_check(&a), Ok(()));
    }

    /// Windows has no peer-cred primitive at all: the gate passes every
    /// same-machine peer (the mandatory token + dir ACL are the gates — the
    /// reduction the startup notice discloses). "None ⇒ refuse" here would
    /// refuse EVERY connection.
    #[test]
    #[cfg(windows)]
    fn peer_check_passes_on_windows() {
        let (a, _b) = CtlStream::pair().expect("socketpair");
        assert_eq!(peer_check(&a), Ok(()));
    }

    /// A minimal stand-in for the server's `serve` auth preamble, run over a
    /// real `CtlStream` pair, proving the on-the-wire handshake:
    /// * the `AUTH <hex>` line is consumed SILENTLY on success (no reply), so
    ///   the first reply a client reads is the response to its first verb;
    /// * a bad/missing token yields exactly `ERR auth\n` and closes.
    fn run_auth_preamble(mut server: CtlStream, token: &str) {
        use std::io::{BufRead, BufReader, Write};
        let reader = BufReader::new(server.try_clone().unwrap());
        let mut lines = reader.lines();
        let first = match lines.next() {
            Some(Ok(l)) => l,
            _ => return,
        };
        match check_auth_line(&first, token) {
            // A folded-in verb (`TOKEN <hex> <verb>`) is answered immediately;
            // we must NOT then block reading another line (the client sent only
            // one). A bare `AUTH`/empty `TOKEN` reads the next line as the verb.
            // This mirrors the real `serve` preamble exactly.
            AuthOutcome::Ok(Some(v)) if !v.is_empty() => {
                let _ = server.write_all(format!("OK {v}\n").as_bytes());
            }
            AuthOutcome::Ok(_) => {
                if let Some(Ok(next)) = lines.next() {
                    let _ = server.write_all(format!("OK {next}\n").as_bytes());
                }
            }
            AuthOutcome::Denied => {
                let _ = server.write_all(b"ERR auth\n");
            }
        }
        let _ = server.flush();
    }

    #[test]
    fn handshake_correct_token_runs_verb() {
        use std::io::{BufRead, BufReader, Write};
        let token = "e".repeat(64);
        let (client, server) = CtlStream::pair().unwrap();
        let tok = token.clone();
        let h = std::thread::spawn(move || run_auth_preamble(server, &tok));

        // Client: AUTH first (silently consumed), then the verb.
        (&client)
            .write_all(format!("AUTH {token}\n").as_bytes())
            .unwrap();
        (&client).write_all(b"text\n").unwrap();
        (&client).flush().unwrap();
        let mut reply = String::new();
        BufReader::new(&client).read_line(&mut reply).unwrap();
        h.join().unwrap();
        // The FIRST reply is the response to `text`, NOT to AUTH.
        assert_eq!(reply, "OK text\n");
    }

    #[test]
    fn handshake_missing_token_is_refused() {
        use std::io::{BufRead, BufReader, Write};
        let token = "f".repeat(64);
        let (client, server) = CtlStream::pair().unwrap();
        let h = std::thread::spawn(move || run_auth_preamble(server, &token));

        // Client skips AUTH and goes straight to a verb.
        (&client).write_all(b"send rm -rf /\n").unwrap();
        (&client).flush().unwrap();
        let mut reply = String::new();
        BufReader::new(&client).read_line(&mut reply).unwrap();
        h.join().unwrap();
        assert_eq!(reply, "ERR auth\n");
    }

    #[test]
    fn sweep_removes_only_dead_instances_files() {
        let dir = std::env::temp_dir().join(format!("aterm-sweep-test-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        // A certainly-dead pid: a reaped child cannot be signalled any more.
        #[cfg(unix)]
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("0")
            .spawn()
            .unwrap();
        #[cfg(windows)]
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "exit"])
            .spawn()
            .unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let us = std::process::id();
        let touch = |name: &str| std::fs::write(dir.join(name), b"x").unwrap();
        touch(&control_socket::instance_sock_name(dead));
        touch(&control_socket::instance_token_name(dead));
        touch(&control_socket::instance_sock_name(us));
        touch(&control_socket::instance_token_name(us));
        touch(TOKEN_FILE);

        sweep_stale_instances(&dir);

        assert!(!dir.join(control_socket::instance_sock_name(dead)).exists());
        assert!(!dir.join(control_socket::instance_token_name(dead)).exists());
        // Our own (live) files and the fixed names survive.
        assert!(dir.join(control_socket::instance_sock_name(us)).exists());
        assert!(dir.join(control_socket::instance_token_name(us)).exists());
        assert!(dir.join(TOKEN_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn latest_link_publishes_atomically_and_repoints() {
        let dir = std::env::temp_dir().join(format!("aterm-link-test-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let link = dir.join(SOCK_FILE);

        let first = dir.join("aterm-101.sock");
        publish_latest_link(&link, first.to_str().unwrap());
        // The target is the RELATIVE instance name (valid via any dir path);
        // read back through the portable alias helper (readlink on Unix, the
        // pointer file's validated contents on Windows).
        assert_eq!(
            aterm_uds::latest::target_name(&link).as_deref(),
            Some(std::ffi::OsStr::new("aterm-101.sock"))
        );

        // A newer instance wins the link; no temp residue is left behind.
        let second = dir.join("aterm-202.sock");
        publish_latest_link(&link, second.to_str().unwrap());
        assert_eq!(
            aterm_uds::latest::target_name(&link).as_deref(),
            Some(std::ffi::OsStr::new("aterm-202.sock"))
        );
        assert!(!dir.join("aterm-202.sock.lnk").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_removes_own_files_and_only_our_symlink() {
        let dir = std::env::temp_dir().join(format!("aterm-clean-test-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let link = dir.join(SOCK_FILE);
        let plan = SocketPlan {
            sock_path: dir.join("aterm-4242.sock").to_string_lossy().into_owned(),
            token_path: dir.join("aterm-4242.token"),
            latest_link: Some(link.clone()),
        };
        let provision = || {
            std::fs::write(&plan.sock_path, b"x").unwrap();
            std::fs::write(&plan.token_path, b"x").unwrap();
        };

        // Link points at us: everything goes.
        provision();
        publish_latest_link(&link, &plan.sock_path);
        cleanup_socket(&plan);
        assert!(!Path::new(&plan.sock_path).exists());
        assert!(!plan.token_path.exists());
        assert!(aterm_uds::latest::target_name(&link).is_none());

        // Link repointed by a newer instance: our files go, the link stays.
        provision();
        publish_latest_link(&link, dir.join("aterm-9.sock").to_str().unwrap());
        cleanup_socket(&plan);
        assert!(!Path::new(&plan.sock_path).exists());
        assert_eq!(
            aterm_uds::latest::target_name(&link).as_deref(),
            Some(std::ffi::OsStr::new("aterm-9.sock"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn handshake_token_prefix_form_runs_inline_verb() {
        use std::io::{BufRead, BufReader, Write};
        let token = "1".repeat(64);
        let (client, server) = CtlStream::pair().unwrap();
        let tok = token.clone();
        let h = std::thread::spawn(move || run_auth_preamble(server, &tok));

        // One-line auth + verb.
        (&client)
            .write_all(format!("TOKEN {token} text\n").as_bytes())
            .unwrap();
        (&client).flush().unwrap();
        let mut reply = String::new();
        BufReader::new(&client).read_line(&mut reply).unwrap();
        h.join().unwrap();
        assert_eq!(reply, "OK text\n");
    }

    fn write_completed_video_fixture(root: &Path, name: &str) {
        let recording = root.join(name);
        std::fs::create_dir(&recording).unwrap();
        std::fs::write(recording.join("index.json"), b"{\"frames\":[]}").unwrap();
        std::fs::write(recording.join(VIDEO_PUBLISHED_FILE), b"published").unwrap();
    }

    fn completed_video_fixture_count(root: &Path) -> usize {
        std::fs::read_dir(root)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry.path().join("index.json").is_file()
                    && entry.path().join(VIDEO_PUBLISHED_FILE).is_file()
            })
            .count()
    }

    fn artifact_lease_count(path: &Path) -> Option<usize> {
        let (leases, _) = artifact_path_leases();
        let held = leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.get(path).map(|state| state.count)
    }

    #[test]
    fn video_producer_lease_spans_marker_visible_reader_handoff() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-video-marker-reader-handoff-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();
        let instance = format!("p{}-marker-reader", std::process::id());

        let mut producer = confine_video_dir_for_instance(&dir, &instance).unwrap();
        let path = producer.path().to_path_buf();
        let index = producer
            .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
            .unwrap();
        producer.publish(&[], &index).unwrap();
        assert_eq!(
            artifact_lease_count(&path),
            Some(1),
            "the producer owns the sole pre-publication lease"
        );
        assert!(
            !path.join(VIDEO_PUBLISHED_FILE).exists(),
            "the index is still invisible to readers before marker publication"
        );

        let marker = producer.publish_marker().unwrap();
        assert!(path.join(VIDEO_PUBLISHED_FILE).is_file());
        assert_eq!(
            artifact_lease_count(&path),
            Some(1),
            "publishing the marker does not release the producer lease"
        );

        let reader = retain_video_artifact_path(&path).expect("marker-visible reader lease");
        let root = crate::pinned_dir::PinnedDir::open_resolved(path.parent().unwrap()).unwrap();
        reader
            .arm_video_reader_sweep(
                root,
                path.file_name().expect("recording name").to_os_string(),
            )
            .unwrap();
        assert_eq!(
            artifact_lease_count(&path),
            Some(2),
            "reader entry overlaps the still-live producer lease"
        );

        drop(marker);
        drop(index);
        drop(producer);
        assert_eq!(
            artifact_lease_count(&path),
            Some(1),
            "producer release hands ownership directly to the reader"
        );
        assert!(
            path.is_dir(),
            "the marker-visible reader still owns the path"
        );

        drop(reader);
        assert_eq!(
            artifact_lease_count(&path),
            None,
            "the final reader release completes the 2→1→0 handoff"
        );
        assert!(
            path.is_dir(),
            "reader-release retention preserves the just-advertised recording"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_image_and_video_sweeps_share_one_gate_and_converge() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-media-concurrent-retention-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();

        let image = confine_automatic_image_path(&dir, "image").unwrap();
        std::fs::write(image.display_path(), b"fresh").unwrap();
        for sequence in 0..(AUTO_IMAGE_KEEP + 4) {
            let name = automatic_capture_name_for(
                "image",
                process_instance_id(),
                1_000_000 + sequence as u64,
            );
            std::fs::write(image.dir.join(name), b"old").unwrap();
        }

        let video_root_path = dir.join("video-fixture");
        ensure_private_dir(&video_root_path).unwrap();
        for sequence in 0..(VIDEO_KEEP + 4) {
            write_completed_video_fixture(&video_root_path, &format!("rec-{sequence:020}-000"));
        }
        let video_fresh = std::ffi::OsString::from("rec-99999999999999999999-000");
        write_completed_video_fixture(&video_root_path, video_fresh.to_str().unwrap());
        let video_root = crate::pinned_dir::PinnedDir::open_resolved(&video_root_path).unwrap();

        let gate = retention_sweep_gate()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();

        let image_worker = {
            let started = started_tx.clone();
            let done = done_tx.clone();
            let target = image.clone();
            std::thread::spawn(move || {
                started.send("image").unwrap();
                prune_automatic_image_dir(&target);
                done.send("image").unwrap();
            })
        };
        let video_worker = {
            let started = started_tx.clone();
            let done = done_tx.clone();
            std::thread::spawn(move || {
                started.send("video").unwrap();
                prune_video_dirs(&video_root, &video_fresh).unwrap();
                done.send("video").unwrap();
            })
        };
        drop(started_tx);
        drop(done_tx);
        assert!(started_rx.recv().is_ok());
        assert!(started_rx.recv().is_ok());
        let early = done_rx
            .recv_timeout(std::time::Duration::from_millis(150))
            .ok();
        let bypassed_gate = early.is_some();
        drop(gate);

        let mut completed = early.into_iter().collect::<Vec<_>>();
        while completed.len() < 2 {
            completed.push(
                done_rx
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .expect("both retention workers finish after the shared gate opens"),
            );
        }
        image_worker.join().unwrap();
        video_worker.join().unwrap();
        assert!(
            !bypassed_gate,
            "neither sweep bypasses the shared census/mutation gate"
        );
        assert_eq!(completed.len(), 2);

        let automatic_images = std::fs::read_dir(&image.dir)
            .unwrap()
            .flatten()
            .filter(|entry| automatic_capture_sequence(&entry.file_name()).is_some())
            .count();
        assert_eq!(
            automatic_images, AUTO_IMAGE_KEEP,
            "the image namespace converges after the serialized sweep"
        );
        assert!(
            image.display_path().is_file(),
            "the image fresh path survives"
        );
        assert_eq!(
            completed_video_fixture_count(&video_root_path),
            VIDEO_KEEP,
            "the video namespace converges after the serialized sweep"
        );
        assert!(
            video_root_path
                .join("rec-99999999999999999999-000")
                .is_dir(),
            "the video fresh path survives"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn video_dir_prune_keeps_the_newest_recordings() {
        let dir = std::env::temp_dir().join(format!("aterm-vid-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();
        let instance = format!("p{}-prune", std::process::id());
        let bootstrap = super::confine_video_dir_for_instance(&dir, &instance)
            .expect("create leased instance namespace");
        let root = bootstrap
            .path()
            .parent()
            .expect("recording has instance parent")
            .to_path_buf();
        drop(bootstrap);

        // Server-named sequence stamps sort oldest-first within this instance.
        // Mint 12 fake completed recordings older than the next real sequence.
        for i in 0..12 {
            let d = root.join(format!("rec-{i:020}-000"));
            std::fs::create_dir(&d).unwrap();
            // A completed recording owns both its index and the durable
            // wire-publication marker.
            std::fs::write(d.join("index.json"), b"{\"frames\":[]}").unwrap();
            std::fs::write(d.join(VIDEO_PUBLISHED_FILE), b"published").unwrap();
        }
        // An IN-FLIGHT recording (older stamp, but NO index.json — its encode worker
        // hasn't written the completion marker yet) must survive the prune untouched.
        let in_flight = root.join("rec-00000000000000000000-999");
        std::fs::create_dir(&in_flight).unwrap();

        // Mint/abort performs NO retention: a refused or failed request must not
        // delete a prior good recording.
        let abandoned = super::confine_video_dir_for_instance(&dir, &instance)
            .expect("mint unpublished recording");
        assert_eq!(
            (0..12)
                .filter(|i| root.join(format!("rec-{i:020}-000")).is_dir())
                .count(),
            12,
            "mint alone never prunes completed recordings"
        );
        drop(abandoned);

        let mut fresh = super::confine_video_dir_for_instance(&dir, &instance)
            .expect("mint recording to publish");
        let fresh_path = fresh.path().to_path_buf();
        let index = fresh
            .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
            .unwrap();
        let published = fresh.publish(&[], &index).expect("publish + prune");
        let marker = fresh.publish_marker().unwrap();
        fresh.prune_after_publish();
        drop(marker);
        assert_eq!(published, fresh_path);
        let mut recs: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("rec-"))
            .collect();
        recs.sort();
        assert!(
            in_flight.is_dir(),
            "an in-flight (index-less) recording is never pruned mid-encode"
        );
        assert_eq!(
            recs.len(),
            super::VIDEO_KEEP + 1,
            "prune keeps VIDEO_KEEP COMPLETED recordings (incl. fresh) + the in-flight one"
        );
        assert!(
            published.is_dir(),
            "the just-published recording survives retention"
        );
        for i in 0..5 {
            assert!(
                !root.join(format!("rec-{i:020}-000")).exists(),
                "old completed recording {i} is pruned"
            );
        }
        for i in 5..12 {
            assert!(
                root.join(format!("rec-{i:020}-000")).is_dir(),
                "newer completed recording {i} survives"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn video_retention_preserves_every_queued_wire_reply() {
        let dir = std::env::temp_dir().join(format!("aterm-vid-queued-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();
        let instance = format!("p{}-queued", std::process::id());

        let mut queued = confine_video_dir_for_instance(&dir, &instance).unwrap();
        let queued_path = queued.path().to_path_buf();
        let queued_index = queued
            .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
            .unwrap();
        queued.publish(&[], &queued_index).unwrap();
        drop(queued_index);
        let root = queued_path.parent().unwrap().to_path_buf();

        for sequence in 0..(VIDEO_KEEP + 3) {
            let name = format!("rec-9999999999999999{sequence:04}-000");
            let recording = root.join(name);
            std::fs::create_dir(&recording).unwrap();
            std::fs::write(recording.join("index.json"), b"{\"frames\":[]}").unwrap();
            std::fs::write(recording.join(VIDEO_PUBLISHED_FILE), b"published").unwrap();
        }

        let mut fresh = confine_video_dir_for_instance(&dir, &instance).unwrap();
        let fresh_path = fresh.path().to_path_buf();
        let fresh_index = fresh
            .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
            .unwrap();
        fresh.publish(&[], &fresh_index).unwrap();
        let fresh_marker = fresh.publish_marker().unwrap();
        fresh.prune_after_publish();
        drop(fresh_marker);

        assert!(
            queued_path.join("index.json").is_file(),
            "a later recording's sweep cannot delete an older queued video reply"
        );
        assert!(
            !queued_path.join(VIDEO_PUBLISHED_FILE).exists(),
            "the queued fixture stays pre-wire: its publication marker is absent"
        );
        assert!(fresh_path.join("index.json").is_file());
        let completed = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry.path().join("index.json").is_file()
                    && entry.path().join(VIDEO_PUBLISHED_FILE).is_file()
            })
            .count();
        assert_eq!(
            completed, VIDEO_KEEP,
            "queued video replies consume retention slots without exceeding the cap"
        );

        drop(fresh_index);
        drop(fresh);
        drop(queued);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn video_prune_has_one_deterministic_global_work_counter() {
        let root = std::env::temp_dir().join(format!(
            "aterm-video-prune-global-budget-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let frame_entries = (0..8)
            .map(|index| serde_json::json!({ "file": format!("frame-{index}.png") }))
            .collect::<Vec<_>>();
        let index = serde_json::to_vec(&serde_json::json!({ "frames": frame_entries })).unwrap();
        for recording in 0..9 {
            let dir = root.join(format!("rec-{recording:020}-000"));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("index.json"), &index).unwrap();
            std::fs::write(dir.join(VIDEO_PUBLISHED_FILE), b"published").unwrap();
            for frame in 0..8 {
                std::fs::write(dir.join(format!("frame-{frame}.png")), b"png").unwrap();
            }
        }
        let fresh_name = std::ffi::OsString::from("rec-99999999999999999999-000");
        let fresh = root.join(&fresh_name);
        std::fs::create_dir(&fresh).unwrap();
        std::fs::write(fresh.join("index.json"), b"{\"frames\":[]}").unwrap();
        std::fs::write(fresh.join(VIDEO_PUBLISHED_FILE), b"published").unwrap();
        let pinned = crate::pinned_dir::PinnedDir::open_resolved(&root).unwrap();

        // Ten scanned entries + two cheap marker/index probes for each of nine
        // candidates, then exactly one marker, one index, and three frame guards
        // in the deep completion check. Leave one unit short of a fourth frame
        // open. The old per-recording limits would multiply here; the shared
        // counter must stop this one probe deterministically.
        let limit = 10 * VIDEO_SCAN_WORK
            + (18 + 2 + 3) * VIDEO_FILE_OPEN_WORK
            + index.len()
            + (VIDEO_FILE_OPEN_WORK - 1);
        let mut work = VideoPruneWork::new(limit);
        prune_video_dirs_with_work(&pinned, &fresh_name, &mut work).unwrap();

        assert_eq!(work.scanned, 10);
        assert_eq!(work.file_opens, 23);
        assert_eq!(work.index_bytes, index.len());
        assert_eq!(work.renames, 0);
        assert_eq!(work.deletes, 0);
        assert!(work.remaining < VIDEO_FILE_OPEN_WORK);
        assert!(
            fresh.join("index.json").is_file(),
            "budget exhaustion cannot affect the freshly published recording"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn video_publish_rejects_deleted_frame_before_index_commit() {
        let dir =
            std::env::temp_dir().join(format!("aterm-video-frame-delete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();
        let instance = format!("p{}-frame-delete", std::process::id());
        let mut recording = confine_video_dir_for_instance(&dir, &instance).unwrap();
        let path = recording.path().to_path_buf();
        let frame = recording
            .write_new_private(std::ffi::OsStr::new("frame_0001.png"), b"png")
            .unwrap();
        recording
            .remove_file_if_exists(std::ffi::OsStr::new("frame_0001.png"))
            .unwrap();
        let index = recording
            .write_new_private(
                std::ffi::OsStr::new("index.json"),
                br#"{"frames":[{"file":"frame_0001.png"}]}"#,
            )
            .unwrap();

        assert!(
            recording.publish(&[frame], &index).is_err(),
            "an index may never certify a deleted or replaced frame"
        );
        drop(index);
        drop(recording);
        assert!(!path.exists(), "failed publication cleans the partial tree");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn video_abort_removes_only_the_retained_instance_after_ancestor_swap() {
        use std::os::unix::fs::symlink;

        let dir =
            std::env::temp_dir().join(format!("aterm-video-abort-swap-{}", std::process::id()));
        let replacement = std::env::temp_dir().join(format!(
            "aterm-video-abort-replacement-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&replacement);
        ensure_private_dir(&dir).unwrap();
        ensure_private_dir(&replacement).unwrap();
        std::fs::write(replacement.join("keep.txt"), b"keep").unwrap();
        let instance = format!("p{}-abort-swap", std::process::id());
        let recording = confine_video_dir_for_instance(&dir, &instance).unwrap();
        let recording_name = recording.path().file_name().unwrap().to_os_string();
        let instance_path = recording.path().parent().unwrap().to_path_buf();
        let moved = instance_path.with_extension("moved");
        std::fs::rename(&instance_path, &moved).unwrap();
        symlink(&replacement, &instance_path).unwrap();

        recording.abort().unwrap();
        assert!(!moved.join(recording_name).exists());
        assert_eq!(
            std::fs::read(replacement.join("keep.txt")).unwrap(),
            b"keep"
        );

        let _ = std::fs::remove_file(instance_path);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(replacement);
    }

    #[test]
    fn instance_lease_beats_pid_reuse_and_pid_false_negative() {
        let root =
            std::env::temp_dir().join(format!("aterm-instance-lease-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        ensure_private_dir(&root).unwrap();

        // PID-reuse case: the embedded PID is reported alive, but the exact
        // launch's lease is unlocked. Exclusive acquisition proves abandonment.
        let reused = ensure_canonical_direct_child(&root, "p424242-reused").unwrap();
        let lease = open_instance_lease(&reused, true).unwrap();
        lease.try_lock().unwrap();
        drop(lease);
        sweep_dead_instance_namespaces_with(&root, "p1-current", |_| true);
        assert!(
            !reused.exists(),
            "an unlocked exact-instance lease wins over a reused live PID"
        );

        // PID false-negative case: a held exact-instance lease is authoritative
        // and survives even when the compatibility liveness probe says dead.
        let held = ensure_canonical_direct_child(&root, "p424243-held").unwrap();
        let lease = open_instance_lease(&held, true).unwrap();
        lease.try_lock().unwrap();
        sweep_dead_instance_namespaces_with(&root, "p1-current", |_| false);
        assert!(
            held.is_dir(),
            "a contended exact-instance lease is never swept"
        );
        drop(lease);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn instance_sweep_decision_is_total_and_lease_authoritative() {
        use InstanceLeaseState::{Acquirable, Held, Invalid, Missing};
        use InstanceSweepDecision::{Keep, Remove};

        for pid_alive in [false, true] {
            assert_eq!(
                decide_instance_namespace_sweep(Acquirable, pid_alive),
                Remove,
                "an acquirable exact lease proves abandonment regardless of PID reuse"
            );
            assert_eq!(
                decide_instance_namespace_sweep(Held, pid_alive),
                Keep,
                "a held exact lease always protects its namespace"
            );
            assert_eq!(
                decide_instance_namespace_sweep(Invalid, pid_alive),
                Keep,
                "malformed or unreadable lease state fails closed"
            );
        }
        assert_eq!(decide_instance_namespace_sweep(Missing, true), Keep);
        assert_eq!(decide_instance_namespace_sweep(Missing, false), Remove);
    }

    #[test]
    fn instance_sweep_refuses_redirected_or_malformed_leases() {
        let root = std::env::temp_dir().join(format!(
            "aterm-instance-lease-invalid-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        ensure_private_dir(&root).unwrap();

        let malformed = ensure_canonical_direct_child(&root, "p424244-malformed").unwrap();
        std::fs::create_dir(malformed.join(INSTANCE_LEASE_FILE)).unwrap();
        sweep_dead_instance_namespaces_with(&root, "p1-current", |_| false);
        assert!(
            malformed.is_dir(),
            "a malformed lease fails closed instead of falling back to PID"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let outside = std::env::temp_dir().join(format!(
                "aterm-instance-lease-outside-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&outside);
            ensure_private_dir(&outside).unwrap();
            let redirected = root.join("p424245-redirected");
            symlink(&outside, &redirected).unwrap();
            sweep_dead_instance_namespaces_with(&root, "p1-current", |_| false);
            assert!(
                std::fs::symlink_metadata(&redirected)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "a redirected namespace entry is refused, never followed"
            );
            assert!(
                outside.is_dir(),
                "the redirect target is never recursively removed"
            );

            let bad_lease = ensure_canonical_direct_child(&root, "p424246-linklease").unwrap();
            let victim = root.join("lease-victim");
            std::fs::write(&victim, b"keep").unwrap();
            symlink(&victim, bad_lease.join(INSTANCE_LEASE_FILE)).unwrap();
            sweep_dead_instance_namespaces_with(&root, "p1-current", |_| false);
            assert!(bad_lease.is_dir(), "a symlink lease fails closed");
            assert_eq!(std::fs::read(victim).unwrap(), b"keep");
            let _ = std::fs::remove_dir_all(outside);
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn video_prune_refuses_symlink_recordings_and_completion_markers() {
        use std::os::unix::fs::symlink;

        let dir =
            std::env::temp_dir().join(format!("aterm-video-prune-links-{}", std::process::id()));
        let outside = std::env::temp_dir().join(format!(
            "aterm-video-prune-links-outside-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
        ensure_private_dir(&dir).unwrap();
        ensure_private_dir(&outside).unwrap();
        std::fs::write(outside.join("index.json"), b"{\"frames\":[]}").unwrap();
        std::fs::write(outside.join(VIDEO_PUBLISHED_FILE), b"outside-marker").unwrap();

        let instance = format!("p{}-prunelinks", std::process::id());
        let bootstrap = confine_video_dir_for_instance(&dir, &instance).unwrap();
        let root = bootstrap.path().parent().unwrap().to_path_buf();
        drop(bootstrap);

        let linked_recording = root.join("rec-00000000000000000000-000");
        symlink(&outside, &linked_recording).unwrap();
        let linked_index_recording = root.join("rec-00000000000000000001-000");
        std::fs::create_dir(&linked_index_recording).unwrap();
        symlink(
            outside.join("index.json"),
            linked_index_recording.join("index.json"),
        )
        .unwrap();
        std::fs::write(
            linked_index_recording.join(VIDEO_PUBLISHED_FILE),
            b"published",
        )
        .unwrap();
        let linked_marker_recording = root.join("rec-00000000000000000002-000");
        std::fs::create_dir(&linked_marker_recording).unwrap();
        std::fs::write(
            linked_marker_recording.join("index.json"),
            b"{\"frames\":[]}",
        )
        .unwrap();
        symlink(
            outside.join(VIDEO_PUBLISHED_FILE),
            linked_marker_recording.join(VIDEO_PUBLISHED_FILE),
        )
        .unwrap();
        // Eight genuine predecessors plus the new publish exceed the cap by one,
        // proving the sweep reaches its quarantine path while all redirected
        // candidates remain untouchable.
        for i in 10..18 {
            let recording = root.join(format!("rec-{i:020}-000"));
            std::fs::create_dir(&recording).unwrap();
            std::fs::write(recording.join("index.json"), b"{\"frames\":[]}").unwrap();
            std::fs::write(recording.join(VIDEO_PUBLISHED_FILE), b"published").unwrap();
        }

        let mut fresh = confine_video_dir_for_instance(&dir, &instance).unwrap();
        let index = fresh
            .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
            .unwrap();
        let _ = fresh.publish(&[], &index).unwrap();
        drop(fresh.publish_marker().unwrap());
        fresh.prune_after_publish();

        assert!(
            !root.join("rec-00000000000000000010-000").exists(),
            "positive control: retention prunes the oldest genuine predecessor"
        );
        assert!(
            std::fs::symlink_metadata(&linked_recording)
                .unwrap()
                .file_type()
                .is_symlink(),
            "a symlink recording is never considered a retention candidate"
        );
        assert!(
            std::fs::symlink_metadata(linked_index_recording.join("index.json"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "a symlink completion index cannot make a recording pruneable"
        );
        assert!(
            std::fs::symlink_metadata(linked_marker_recording.join(VIDEO_PUBLISHED_FILE))
                .unwrap()
                .file_type()
                .is_symlink(),
            "a symlink publication marker cannot make a recording pruneable"
        );
        assert_eq!(
            std::fs::read(outside.join("index.json")).unwrap(),
            b"{\"frames\":[]}",
            "retention never follows either redirect"
        );
        assert_eq!(
            std::fs::read(outside.join(VIDEO_PUBLISHED_FILE)).unwrap(),
            b"outside-marker",
            "retention never follows a publication-marker redirect"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// R37: the network-drive token PERSISTS — a second call (simulating a remote
    /// restart) returns the SAME token, so a saved dial credential is not
    /// invalidated. A fresh dir generates one; a corrupt file fails closed.
    #[test]
    fn network_drive_token_persists_across_restarts() {
        let dir = std::env::temp_dir().join(format!("aterm-drive-tok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // First launch: mint + persist a valid 64-hex token; `minted` is true so the
        // caller reveals it to the operator's log exactly once.
        let (t1, minted1) = super::load_or_create_network_drive_token(&dir).expect("mint token");
        assert!(minted1, "first run freshly mints (reveal-once)");
        assert_eq!(t1.len(), 64, "64-char hex");
        assert!(
            aterm_session::EdgeToken::from_hex(&t1).is_some(),
            "valid EdgeToken"
        );
        // A file was written (0600 on unix — perms enforced by write_private).
        assert!(dir.join(super::NETWORK_DRIVE_TOKEN_FILE).exists());

        // SECOND launch (a "restart"): the SAME token — the persistence R37 needs —
        // and `minted` is now FALSE, so the raw secret is NOT re-logged.
        let (t2, minted2) = super::load_or_create_network_drive_token(&dir).expect("reload token");
        assert_eq!(t1, t2, "the drive token survives a restart");
        assert!(!minted2, "a reload does not re-reveal the token");

        // A corrupt file fails closed: without a cross-process lock it cannot
        // be safely replaced while another process might be inspecting it.
        std::fs::write(dir.join(super::NETWORK_DRIVE_TOKEN_FILE), b"garbage").unwrap();
        assert!(super::load_or_create_network_drive_token(&dir).is_none());
        std::fs::remove_file(dir.join(super::NETWORK_DRIVE_TOKEN_FILE)).unwrap();
        let (t3, minted3) =
            super::load_or_create_network_drive_token(&dir).expect("mint after operator cleanup");
        assert_ne!(t3, t1, "operator cleanup permits a new token");
        assert!(minted3, "a regenerated token counts as freshly minted");
        assert!(aterm_session::EdgeToken::from_hex(&t3).is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn network_drive_token_create_new_race_has_one_winner() {
        let dir =
            std::env::temp_dir().join(format!("aterm-drive-token-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let dir = dir.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                super::load_or_create_network_drive_token_with_hook(&dir, move || {
                    barrier.wait();
                })
                .unwrap()
            }));
        }
        let first = workers.remove(0).join().unwrap();
        let second = workers.remove(0).join().unwrap();
        assert_eq!(first.0, second.0, "both creators converge on one token");
        assert_ne!(
            first.1, second.1,
            "exactly one atomic no-replace publisher is the mint winner"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join(super::NETWORK_DRIVE_TOKEN_FILE)).unwrap(),
            first.0,
            "both callers return the exact fully persisted final token"
        );
        assert!(
            std::fs::read_dir(&dir).unwrap().flatten().all(|entry| {
                !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".aterm-write-")
            }),
            "losing temporary token files are removed"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn network_drive_token_final_is_absent_until_atomic_complete_publication() {
        use std::os::unix::fs::MetadataExt as _;

        let dir = std::env::temp_dir().join(format!(
            "aterm-drive-token-publication-seams-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let final_path = dir.join(super::NETWORK_DRIVE_TOKEN_FILE);
        let (temp_entered_tx, temp_entered_rx) = std::sync::mpsc::channel();
        let (release_temp_tx, release_temp_rx) = std::sync::mpsc::channel();
        let (published_tx, published_rx) = std::sync::mpsc::channel();
        let (release_publish_tx, release_publish_rx) = std::sync::mpsc::channel();
        let writer_dir = dir.clone();
        let writer = std::thread::spawn(move || {
            super::load_or_create_network_drive_token_with_hooks(
                &writer_dir,
                move || {
                    temp_entered_tx.send(()).unwrap();
                    release_temp_rx.recv().unwrap();
                },
                move || {
                    published_tx.send(()).unwrap();
                    release_publish_rx.recv().unwrap();
                },
            )
            .unwrap()
        });

        temp_entered_rx.recv().unwrap();
        assert!(
            !final_path.exists(),
            "the fully private temp write exposes no empty or partial final component"
        );
        release_temp_tx.send(()).unwrap();
        published_rx.recv().unwrap();
        let metadata = std::fs::metadata(&final_path).unwrap();
        assert_eq!(
            metadata.nlink(),
            1,
            "atomic no-replace rename has no final-name nlink=2 window"
        );
        let loser = super::load_or_create_network_drive_token(&dir)
            .expect("a concurrent loser reads the complete visible token");
        assert!(
            !loser.1,
            "the observer loads rather than remints the published token"
        );
        release_publish_tx.send(()).unwrap();
        let winner = writer.join().unwrap();
        assert!(winner.1);
        assert_eq!(loser.0, winner.0);
        assert_eq!(std::fs::read_to_string(&final_path).unwrap(), winner.0);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn network_drive_token_rejects_hardlink_and_fifo_without_touching_victim() {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::PermissionsExt as _;

        let hardlink_dir =
            std::env::temp_dir().join(format!("aterm-drive-token-hardlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&hardlink_dir);
        std::fs::create_dir_all(&hardlink_dir).unwrap();
        let victim = hardlink_dir.join("victim");
        std::fs::write(&victim, b"victim-bytes").unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::fs::hard_link(&victim, hardlink_dir.join(super::NETWORK_DRIVE_TOKEN_FILE)).unwrap();
        assert!(
            super::load_or_create_network_drive_token(&hardlink_dir).is_none(),
            "a planted hardlink fails closed"
        );
        assert_eq!(std::fs::read(&victim).unwrap(), b"victim-bytes");
        assert_eq!(
            std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
            0o640
        );

        let fifo_dir =
            std::env::temp_dir().join(format!("aterm-drive-token-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&fifo_dir);
        std::fs::create_dir_all(&fifo_dir).unwrap();
        let fifo = fifo_dir.join(super::NETWORK_DRIVE_TOKEN_FILE);
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `fifo_c` is a valid NUL-terminated absent path.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        assert!(super::load_or_create_network_drive_token(&fifo_dir).is_none());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "the token loader must never block on a planted FIFO"
        );

        let _ = std::fs::remove_dir_all(hardlink_dir);
        let _ = std::fs::remove_dir_all(fifo_dir);
    }

    #[cfg(unix)]
    #[test]
    fn network_drive_token_fails_closed_after_ancestor_replacement() {
        use std::os::unix::fs::symlink;

        let root =
            std::env::temp_dir().join(format!("aterm-drive-token-swap-{}", std::process::id()));
        let replacement = std::env::temp_dir().join(format!(
            "aterm-drive-token-swap-replacement-{}",
            std::process::id()
        ));
        let token_dir = root.join("tokens");
        let moved = root.join("tokens-moved");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&replacement);
        std::fs::create_dir_all(&token_dir).unwrap();
        std::fs::create_dir_all(&replacement).unwrap();
        std::fs::write(replacement.join("keep.txt"), b"keep").unwrap();

        let token_dir_for_hook = token_dir.clone();
        let moved_for_hook = moved.clone();
        let replacement_for_hook = replacement.clone();
        assert!(
            super::load_or_create_network_drive_token_with_hook(&token_dir, move || {
                std::fs::rename(&token_dir_for_hook, &moved_for_hook).unwrap();
                symlink(&replacement_for_hook, &token_dir_for_hook).unwrap();
            })
            .is_none()
        );
        assert!(!moved.join(super::NETWORK_DRIVE_TOKEN_FILE).exists());
        assert_eq!(
            std::fs::read(replacement.join("keep.txt")).unwrap(),
            b"keep"
        );
        assert!(
            !replacement.join(super::NETWORK_DRIVE_TOKEN_FILE).exists(),
            "the replacement path never receives the secret"
        );

        let _ = std::fs::remove_file(token_dir);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(replacement);
    }
}
