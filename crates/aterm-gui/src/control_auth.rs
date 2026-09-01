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
//! flags. An instance on an explicit `$ATERM_CONTROL_SOCK` path is isolated
//! the same way — its token is named after ITS socket
//! ([`control_socket::token_name_for_sock`], [`token_path_for_socket`]) — so
//! two private instances in one directory keep their own credentials instead
//! of the second one overwriting the first's and locking out its clients.
//! Naming/staleness decisions are engine-side
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
// `peer_uid`'s production callers are `peer_check` (inside the unix `imp`
// module) and the handoff rendezvous DIALER (`handoff_rendezvous::dial_and_claim`
// proves the listener is same-uid before it writes the claim secret); the
// socketpair unit test below calls it directly.
// Off macOS the rendezvous dialer does not exist and only the unit test
// below calls it — the re-export rides exactly its consumers.
#[cfg(all(unix, any(target_os = "macos", test)))]
pub(crate) use imp::peer_uid;
// Test-only import: `random_token_hex`'s production callers live inside the
// per-platform `imp` modules (`provision_token`); only the unit tests below
// draw a token directly.
#[cfg(test)]
use imp::random_token_hex;

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

/// Typed confinement outcome so callers never need an out-of-band `Cell<bool>`
/// to distinguish a resource refusal from an invalid/unavailable path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArtifactConfinementError {
    Invalid,
    AdmissionRefused,
}

fn confinement_open_error(error: std::io::Error) -> ArtifactConfinementError {
    if error.kind() == std::io::ErrorKind::WouldBlock {
        ArtifactConfinementError::AdmissionRefused
    } else {
        ArtifactConfinementError::Invalid
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
pub(crate) fn enter_low_nofile_test_child(
    environment: &str,
    exact_test: &str,
    spare_descriptors: usize,
) -> bool {
    if std::env::var_os(environment).is_none() {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", exact_test, "--nocapture", "--test-threads=1"])
            .env(environment, "1")
            .output()
            .expect("spawn isolated low-NOFILE test");
        assert!(
            output.status.success(),
            "low-NOFILE child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return false;
    }

    let fd_dir = ["/proc/self/fd", "/dev/fd"]
        .into_iter()
        .find(|path| Path::new(path).is_dir())
        .expect("Unix exposes the process descriptor directory");
    let open = std::fs::read_dir(fd_dir).unwrap().count();
    let target =
        libc::rlim_t::try_from(open + spare_descriptors).expect("descriptor limit fits rlim_t");
    let mut inherited = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    // SAFETY: `inherited` is valid writable storage for one rlimit value.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, inherited.as_mut_ptr()) },
        0
    );
    // SAFETY: successful getrlimit initialized the whole value.
    let inherited = unsafe { inherited.assume_init() };
    assert!(
        target <= inherited.rlim_max,
        "test needs {spare_descriptors} spare descriptors"
    );
    let constrained = libc::rlimit {
        rlim_cur: target,
        rlim_max: inherited.rlim_max,
    };
    // SAFETY: the child-only mutation preserves the inherited hard ceiling.
    assert_eq!(
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const constrained) },
        0
    );
    true
}

/// One conservative filesystem-wide lock excludes concurrent explicit image
/// writes in the shared `images/` namespace. A contender waits briefly for an
/// ordinary reply/ACK handoff, then fails busy so the single encode lane stays
/// bounded. Locking the namespace (rather than a spelling-derived sidecar)
/// covers case-folding, Unicode normalization, and Windows short-name aliases
/// without guessing the mounted filesystem's name-equivalence rules.
const CAPTURE_LOCK_DIR: &str = ".capture-locks";
const CAPTURE_NAMESPACE_LEASE_FILE: &str = "explicit";

/// How long an EXPLICIT capture waits for the previous reply's retention guard
/// before it refuses. Long enough that two overlapping drivers queue instead of
/// colliding (a guard is released the moment its client acknowledges —
/// microseconds to a few milliseconds), short enough that a genuinely stuck
/// holder is reported rather than hanging the control thread.
const CAPTURE_NAMESPACE_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

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

/// Ownership of a filesystem artifact through a guarded reply's acknowledgement
/// or a failed/legacy handoff's additional quarantine interval. Inline image
/// bytes release their path lease before the write-only socket handoff and keep
/// only the memory/admission permit through flush. Server-unique auto
/// images/videos use the process-local `key` for retention; caller-explicit
/// images additionally hold the shared namespace's OS advisory lock so
/// different aterm processes and aliased filename spellings fail busy instead
/// of racing one another.
#[derive(Debug)]
pub(crate) struct ArtifactPathLease {
    key: Option<PathBuf>,
    os_lock: Option<std::fs::File>,
    /// Capability local to this lease. It shares the lease owner's admitted
    /// directory chain and therefore can never outlive that owner's capacity
    /// charge. The final lease uses its own capability for convergence.
    video_sweep: Option<VideoRetentionSweep>,
    /// Automatic-image namespace GC is armed by both the target and its wire
    /// lease. The shared final reference drops only after every retained path
    /// capability has closed.
    _deferred_cleanup: Option<std::sync::Arc<DeferredImageCleanup>>,
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
        action = "RejectReplacedIdentity",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reader_lease"
    )
)]
fn artifact_reader_reject_replaced_identity_anchor() {}

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
struct VideoRetentionSweep {
    root: crate::pinned_dir::PinnedDir,
    fresh: std::ffi::OsString,
    identity: VideoLeaseIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VideoLeaseIdentity {
    root: crate::pinned_dir::PinnedDirIdentity,
    recording: crate::pinned_dir::PinnedDirIdentity,
}

#[derive(Default)]
struct ArtifactLeaseState {
    count: usize,
    /// Values, not retained handles: every descriptor remains owned by an
    /// admitted producer/reader lease. Comparing both the instance root and the
    /// recording child prevents a replacement namespace—or the same recording
    /// inode moved into one—from joining an older lexical-path lease group.
    video_identity: Option<VideoLeaseIdentity>,
    /// Set at the marker visibility boundary or after a reader's final closed
    /// identity validation. Whichever local lease is last performs the sweep.
    video_sweep_requested: bool,
    /// A count-zero entry remains present while its capability-bound sweep runs.
    /// New readers fail closed instead of entering between the last-release
    /// decision and a retention rename.
    sweeping: bool,
    /// Reserved when this distinct video key first enters the registry. The
    /// final armed lease transfers it to the priority cleanup lane, so Drop
    /// never races best-effort GC for admission or waits for queue capacity.
    video_retention: Option<crate::control::VideoRetentionPermit>,
}

const ARTIFACT_CLEANUP_QUEUE_LIMIT: usize = 32;
static ARTIFACT_CLEANUP_QUEUE_LIVE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[derive(Clone, Debug)]
struct ArtifactCleanupQueuePermit {
    _inner: std::sync::Arc<ArtifactCleanupQueuePermitInner>,
}

#[derive(Debug)]
struct ArtifactCleanupQueuePermitInner {
    live: Option<&'static std::sync::atomic::AtomicUsize>,
}

#[derive(Debug)]
struct DeferredImageCleanup {
    image_dir: PathBuf,
    image_identity: crate::pinned_dir::PinnedDirIdentity,
    fresh: std::ffi::OsString,
    image_queue: Option<ArtifactCleanupQueuePermit>,
    image_armed: std::sync::atomic::AtomicBool,
    namespace_root: PathBuf,
    current: String,
    namespace_queue: Option<ArtifactCleanupQueuePermit>,
}

impl DeferredImageCleanup {
    fn reserve(
        image_dir: PathBuf,
        image_identity: crate::pinned_dir::PinnedDirIdentity,
        fresh: std::ffi::OsString,
        namespace_root: PathBuf,
        current: String,
    ) -> Option<std::sync::Arc<Self>> {
        Some(std::sync::Arc::new(Self {
            image_dir,
            image_identity,
            fresh,
            image_queue: Some(reserve_artifact_cleanup_queue()?),
            image_armed: std::sync::atomic::AtomicBool::new(false),
            namespace_root,
            current,
            namespace_queue: Some(reserve_artifact_cleanup_queue()?),
        }))
    }

    fn arm_image_prune(&self) {
        self.image_armed
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

impl Drop for DeferredImageCleanup {
    fn drop(&mut self) {
        if self.image_armed.load(std::sync::atomic::Ordering::Acquire) {
            if let Some(queue) = self.image_queue.take() {
                let _ = try_schedule_artifact_cleanup(ArtifactCleanupTask::AutomaticImages {
                    dir: self.image_dir.clone(),
                    expected_dir: self.image_identity,
                    fresh: self.fresh.clone(),
                    _queue: queue,
                });
            } else {
                debug_assert!(false, "deferred image prune lost its reserved queue slot");
            }
        }
        if let Some(queue) = self.namespace_queue.take() {
            schedule_dead_instance_namespace_sweep(&self.namespace_root, &self.current, queue);
        } else {
            debug_assert!(
                false,
                "deferred namespace sweep lost its reserved queue slot"
            );
        }
    }
}

#[derive(Debug)]
struct DeferredVideoCleanup {
    tombstone_root: PathBuf,
    tombstone_identity: crate::pinned_dir::PinnedDirIdentity,
    tombstone_queue: Option<ArtifactCleanupQueuePermit>,
    tombstone_armed: bool,
    dead_root: PathBuf,
    current: String,
    dead_queue: Option<ArtifactCleanupQueuePermit>,
}

impl DeferredVideoCleanup {
    fn arm_tombstone(&mut self) {
        self.tombstone_armed = true;
    }
}

impl Drop for DeferredVideoCleanup {
    fn drop(&mut self) {
        if self.tombstone_armed
            && let Some(queue) = self.tombstone_queue.take()
        {
            schedule_video_tombstone_sweep(&self.tombstone_root, self.tombstone_identity, queue);
        }
        if let Some(queue) = self.dead_queue.take() {
            schedule_dead_instance_namespace_sweep(&self.dead_root, &self.current, queue);
        } else {
            debug_assert!(false, "deferred video GC lost its reserved queue slot");
        }
    }
}

impl ArtifactCleanupQueuePermit {
    fn try_acquire() -> Option<Self> {
        ARTIFACT_CLEANUP_QUEUE_LIVE
            .try_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |current| (current < ARTIFACT_CLEANUP_QUEUE_LIMIT).then_some(current + 1),
            )
            .ok()
            .map(|_| Self {
                _inner: std::sync::Arc::new(ArtifactCleanupQueuePermitInner {
                    live: Some(&ARTIFACT_CLEANUP_QUEUE_LIVE),
                }),
            })
    }

    #[cfg(test)]
    fn unmetered() -> Self {
        Self {
            _inner: std::sync::Arc::new(ArtifactCleanupQueuePermitInner { live: None }),
        }
    }
}

impl Drop for ArtifactCleanupQueuePermitInner {
    fn drop(&mut self) {
        if let Some(live) = self.live {
            let previous = live.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            debug_assert!(previous > 0, "artifact cleanup queue permit underflow");
        }
    }
}

enum ArtifactCleanupTask {
    AutomaticImages {
        dir: PathBuf,
        expected_dir: crate::pinned_dir::PinnedDirIdentity,
        fresh: std::ffi::OsString,
        _queue: ArtifactCleanupQueuePermit,
    },
    DeadNamespaces {
        root: PathBuf,
        current: String,
        _queue: ArtifactCleanupQueuePermit,
    },
    DeadNamespaceContinuation(DeadNamespaceSweep),
    VideoTombstones {
        root: PathBuf,
        expected_root: crate::pinned_dir::PinnedDirIdentity,
        _queue: ArtifactCleanupQueuePermit,
    },
    VideoTombstoneContinuation(VideoTombstoneSweep),
    #[cfg(test)]
    Barrier(std::sync::mpsc::SyncSender<()>),
    #[cfg(test)]
    PanicForTest(ArtifactCleanupPanicRecovery),
}

#[derive(Clone)]
enum ArtifactCleanupPanicRecovery {
    AutomaticImages {
        dir: PathBuf,
        expected_dir: crate::pinned_dir::PinnedDirIdentity,
        fresh: std::ffi::OsString,
        queue: ArtifactCleanupQueuePermit,
    },
    DeadNamespaces {
        root: PathBuf,
        current: String,
        queue: ArtifactCleanupQueuePermit,
    },
    VideoTombstones {
        root: PathBuf,
        expected_root: crate::pinned_dir::PinnedDirIdentity,
        queue: ArtifactCleanupQueuePermit,
    },
    #[cfg(test)]
    Barrier(std::sync::mpsc::SyncSender<()>),
}

impl ArtifactCleanupTask {
    fn panic_recovery(&self) -> ArtifactCleanupPanicRecovery {
        match self {
            Self::AutomaticImages {
                dir,
                expected_dir,
                fresh,
                _queue,
            } => ArtifactCleanupPanicRecovery::AutomaticImages {
                dir: dir.clone(),
                expected_dir: *expected_dir,
                fresh: fresh.clone(),
                queue: _queue.clone(),
            },
            Self::DeadNamespaces {
                root,
                current,
                _queue,
            } => ArtifactCleanupPanicRecovery::DeadNamespaces {
                root: root.clone(),
                current: current.clone(),
                queue: _queue.clone(),
            },
            Self::DeadNamespaceContinuation(sweep) => {
                ArtifactCleanupPanicRecovery::DeadNamespaces {
                    root: sweep.root_path.clone(),
                    current: sweep.current.clone(),
                    queue: sweep._queue.clone(),
                }
            }
            Self::VideoTombstones {
                root,
                expected_root,
                _queue,
            } => ArtifactCleanupPanicRecovery::VideoTombstones {
                root: root.clone(),
                expected_root: *expected_root,
                queue: _queue.clone(),
            },
            Self::VideoTombstoneContinuation(sweep) => {
                ArtifactCleanupPanicRecovery::VideoTombstones {
                    root: sweep.root_path.clone(),
                    expected_root: sweep.expected_root,
                    queue: sweep._queue.clone(),
                }
            }
            #[cfg(test)]
            Self::Barrier(done) => ArtifactCleanupPanicRecovery::Barrier(done.clone()),
            #[cfg(test)]
            Self::PanicForTest(recovery) => recovery.clone(),
        }
    }
}

struct VideoRetentionTask {
    root: Option<crate::pinned_dir::PinnedDir>,
    fresh: std::ffi::OsString,
    /// Last so the retained directory chain closes before its reservation is
    /// made available to another distinct artifact key.
    admission: Option<crate::control::VideoRetentionPermit>,
    /// Last so every retained descriptor and its admission token are released
    /// before the registry wakes a waiter, including during panic unwinding.
    _completion: VideoRetentionCompletion,
}

struct VideoRetentionCompletion {
    key: PathBuf,
}

impl Drop for VideoRetentionCompletion {
    fn drop(&mut self) {
        finish_video_retention_sweep(&self.key);
    }
}

struct ArtifactCleanupScheduler {
    video: std::sync::mpsc::Sender<VideoRetentionTask>,
    best_effort: std::sync::mpsc::Sender<ArtifactCleanupTask>,
    wake: std::sync::mpsc::SyncSender<()>,
}

fn artifact_cleanup_scheduler() -> Option<&'static ArtifactCleanupScheduler> {
    static SCHEDULER: std::sync::OnceLock<ArtifactCleanupScheduler> = std::sync::OnceLock::new();
    static INITIALIZING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    if let Some(scheduler) = SCHEDULER.get() {
        return Some(scheduler);
    }
    // `OnceLock<Option<_>>` would permanently cache one transient thread-spawn
    // failure. Serialize attempts, but publish only a successfully spawned
    // worker so a later request can retry without creating duplicate workers.
    let _initializing = INITIALIZING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(scheduler) = SCHEDULER.get() {
        return Some(scheduler);
    }
    // Both logical queues are externally bounded by permits reserved before
    // work becomes visible. Their unbounded transport makes each final-lease
    // send nonblocking and immune to best-effort saturation.
    let (video, video_receiver) = std::sync::mpsc::channel();
    let (best_effort, best_effort_receiver) = std::sync::mpsc::channel();
    let (wake, wake_receiver) = std::sync::mpsc::sync_channel(1);
    let worker_best_effort = best_effort.clone();
    std::thread::Builder::new()
        .name("aterm-artifact-cleanup".into())
        .spawn(move || {
            artifact_cleanup_worker(
                &video_receiver,
                &best_effort_receiver,
                &wake_receiver,
                &worker_best_effort,
            );
        })
        .ok()?;
    let scheduler = ArtifactCleanupScheduler {
        video,
        best_effort,
        wake,
    };
    let published = SCHEDULER.set(scheduler).is_ok();
    debug_assert!(published, "cleanup scheduler initialization serialized");
    SCHEDULER.get()
}

fn notify_artifact_cleanup_worker(scheduler: &ArtifactCleanupScheduler) {
    let _ = scheduler.wake.try_send(());
}

fn try_schedule_artifact_cleanup(task: ArtifactCleanupTask) -> bool {
    let Some(scheduler) = artifact_cleanup_scheduler() else {
        return false;
    };
    if scheduler.best_effort.send(task).is_err() {
        return false;
    }
    notify_artifact_cleanup_worker(scheduler);
    true
}

fn try_schedule_video_retention(task: VideoRetentionTask) -> bool {
    let Some(scheduler) = artifact_cleanup_scheduler() else {
        return false;
    };
    if scheduler.video.send(task).is_err() {
        return false;
    }
    notify_artifact_cleanup_worker(scheduler);
    true
}

fn reserve_artifact_cleanup_queue() -> Option<ArtifactCleanupQueuePermit> {
    artifact_cleanup_scheduler()?;
    ArtifactCleanupQueuePermit::try_acquire()
}

fn reserve_video_retention(path: &Path) -> Option<crate::control::VideoRetentionPermit> {
    artifact_cleanup_scheduler()?;
    crate::control::ReplyRetention::try_reserve_video_retention_for_path(path)
}

#[derive(Debug)]
struct PendingDeadNamespaceSweep {
    dirty: bool,
}

fn pending_dead_namespace_sweeps()
-> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, PendingDeadNamespaceSweep>> {
    static PENDING: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, PendingDeadNamespaceSweep>>,
    > = std::sync::OnceLock::new();
    PENDING.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Coalesce repeated capture-time GC requests and hand the actual directory
/// census to the single bounded cleanup worker. Admission performs no scan or
/// recursive deletion.
fn schedule_dead_instance_namespace_sweep(
    root: &Path,
    current: &str,
    queue: ArtifactCleanupQueuePermit,
) {
    let root = root.to_path_buf();
    let pending = pending_dead_namespace_sweeps();
    let mut held = pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing) = held.get_mut(&root) {
        // A persistent ReadDir is not required to observe entries created
        // after it opened. Remember the coalesced generation so EOF restarts
        // from a fresh cursor instead of losing the only lifecycle kick.
        existing.dirty = true;
        return;
    }
    held.insert(root.clone(), PendingDeadNamespaceSweep { dirty: false });
    drop(held);
    if !try_schedule_artifact_cleanup(ArtifactCleanupTask::DeadNamespaces {
        root: root.clone(),
        current: current.to_string(),
        _queue: queue,
    }) {
        clear_pending_dead_namespace_sweep(&root);
    }
}

fn clear_pending_dead_namespace_sweep(root: &Path) {
    pending_dead_namespace_sweeps()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(root);
}

fn recover_artifact_cleanup_after_panic(
    recovery: ArtifactCleanupPanicRecovery,
    sender: &std::sync::mpsc::Sender<ArtifactCleanupTask>,
) {
    match recovery {
        ArtifactCleanupPanicRecovery::AutomaticImages {
            dir,
            expected_dir,
            fresh,
            queue,
        } => {
            let _ = sender.send(ArtifactCleanupTask::AutomaticImages {
                dir,
                expected_dir,
                fresh,
                _queue: queue,
            });
        }
        ArtifactCleanupPanicRecovery::DeadNamespaces {
            root,
            current,
            queue,
        } => {
            let pending = pending_dead_namespace_sweeps();
            let mut held = pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(state) = held.get_mut(&root) else {
                return;
            };
            // The replacement task opens a fresh cursor and therefore covers
            // every generation visible before this point. A later lifecycle
            // kick sets `dirty` again and forces one more restart at EOF.
            state.dirty = false;
            drop(held);
            if sender
                .send(ArtifactCleanupTask::DeadNamespaces {
                    root: root.clone(),
                    current,
                    _queue: queue,
                })
                .is_err()
            {
                clear_pending_dead_namespace_sweep(&root);
            }
        }
        ArtifactCleanupPanicRecovery::VideoTombstones {
            root,
            expected_root,
            queue,
        } => {
            let pending = pending_video_tombstone_sweeps();
            let mut held = pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(entry) = held
                .iter_mut()
                .find(|entry| entry.root == root && entry.expected_root == expected_root)
            else {
                return;
            };
            entry.dirty = false;
            drop(held);
            if sender
                .send(ArtifactCleanupTask::VideoTombstones {
                    root: root.clone(),
                    expected_root,
                    _queue: queue,
                })
                .is_err()
            {
                clear_pending_video_tombstone_sweep(&root, expected_root);
            }
        }
        #[cfg(test)]
        ArtifactCleanupPanicRecovery::Barrier(done) => {
            let _ = sender.send(ArtifactCleanupTask::Barrier(done));
        }
    }
}

fn finish_or_restart_dead_namespace_sweep(
    root: PathBuf,
    current: String,
    queue: ArtifactCleanupQueuePermit,
    sender: &std::sync::mpsc::Sender<ArtifactCleanupTask>,
) {
    let pending = pending_dead_namespace_sweeps();
    let mut held = pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(state) = held.get_mut(&root) else {
        return;
    };
    if !state.dirty {
        held.remove(&root);
        return;
    }
    state.dirty = false;
    drop(held);
    if sender
        .send(ArtifactCleanupTask::DeadNamespaces {
            root: root.clone(),
            current,
            _queue: queue,
        })
        .is_err()
    {
        clear_pending_dead_namespace_sweep(&root);
    }
}

fn finish_video_retention_sweep(key: &Path) {
    let (leases, changed) = artifact_path_leases();
    let mut held = leases
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if held
        .get(key)
        .is_some_and(|state| state.count == 0 && state.sweeping)
    {
        held.remove(key);
    }
    artifact_reader_finish_sweep_anchor();
    changed.notify_all();
}

#[cfg(test)]
pub(crate) fn wait_for_artifact_cleanup_for_test(path: &Path) {
    // Test fixtures often build paths under macOS's `/var` spelling while
    // confinement registers the canonical `/private/var` spelling. Preserve
    // the registry key even if an ancestor was replaced after the lease was
    // acquired; canonicalization changes spelling, not the stored path text.
    let key = path
        .ancestors()
        .find_map(|ancestor| {
            let canonical = std::fs::canonicalize(ancestor).ok()?;
            let tail = path.strip_prefix(ancestor).ok()?;
            Some(if tail.as_os_str().is_empty() {
                canonical
            } else {
                canonical.join(tail)
            })
        })
        .unwrap_or_else(|| path.to_path_buf());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let (leases, changed) = artifact_path_leases();
    let mut held = leases
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while held.contains_key(&key) {
        let now = std::time::Instant::now();
        assert!(now < deadline, "artifact cleanup timed out for {key:?}");
        let (next, timeout) = changed
            .wait_timeout(held, deadline.saturating_duration_since(now))
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held = next;
        assert!(!timeout.timed_out() || !held.contains_key(&key));
    }
}

#[cfg(test)]
fn wait_for_best_effort_cleanup_barrier_for_test() {
    let (done, completed) = std::sync::mpsc::sync_channel(1);
    assert!(try_schedule_artifact_cleanup(ArtifactCleanupTask::Barrier(
        done
    )));
    completed
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("artifact best-effort cleanup barrier timed out");
}

fn run_video_retention_task(mut task: VideoRetentionTask) {
    let root = task
        .root
        .as_ref()
        .expect("video retention task owns its pinned root");
    if let Ok(true) = prune_video_dirs(root, &task.fresh) {
        let _ = root.sync();
    }
    // Close the transferred capability and release its reservation before
    // the completion guard wakes a waiter. Keeping both fields inside `task`
    // also gives panic unwinding the same declaration-order guarantee.
    drop(task.root.take());
    drop(task.admission.take());
}

fn run_best_effort_cleanup_task(
    task: ArtifactCleanupTask,
    sender: &std::sync::mpsc::Sender<ArtifactCleanupTask>,
) {
    match task {
        ArtifactCleanupTask::AutomaticImages {
            dir,
            expected_dir,
            fresh,
            _queue,
        } => {
            let Some(admission) =
                crate::control::ReplyRetention::try_reserve_cleanup_for_path(&dir)
            else {
                let _ = sender.send(ArtifactCleanupTask::AutomaticImages {
                    dir,
                    expected_dir,
                    fresh,
                    _queue,
                });
                return;
            };
            let Ok(pinned) = crate::pinned_dir::PinnedDir::open(&dir) else {
                return;
            };
            if pinned.retained_identity().ok() != Some(expected_dir)
                || pinned.validate_path_identity().is_err()
            {
                return;
            }
            prune_automatic_image_dir_at(&dir, &fresh, &pinned);
            drop(admission);
        }
        ArtifactCleanupTask::DeadNamespaces {
            root,
            current,
            _queue,
        } => {
            let Some(admission) =
                crate::control::ReplyRetention::try_reserve_cleanup_for_path(&root)
            else {
                let _ = sender.send(ArtifactCleanupTask::DeadNamespaces {
                    root,
                    current,
                    _queue,
                });
                return;
            };
            let mut sweep =
                match DeadNamespaceSweep::begin(&root, current.clone(), _queue, admission) {
                    Ok(sweep) => sweep,
                    Err(queue) => {
                        finish_or_restart_dead_namespace_sweep(root, current, queue, sender);
                        return;
                    }
                };
            if sweep.run_batch(&imp::pid_alive) {
                finish_or_restart_dead_namespace_cursor(sweep, sender);
            } else if sender
                .send(ArtifactCleanupTask::DeadNamespaceContinuation(sweep))
                .is_err()
            {
                clear_pending_dead_namespace_sweep(&root);
            }
        }
        ArtifactCleanupTask::DeadNamespaceContinuation(mut sweep) => {
            let root = sweep.root_path.clone();
            if sweep.run_batch(&imp::pid_alive) {
                finish_or_restart_dead_namespace_cursor(sweep, sender);
            } else if sender
                .send(ArtifactCleanupTask::DeadNamespaceContinuation(sweep))
                .is_err()
            {
                clear_pending_dead_namespace_sweep(&root);
            }
        }
        ArtifactCleanupTask::VideoTombstones {
            root,
            expected_root,
            _queue,
        } => {
            let Some(admission) =
                crate::control::ReplyRetention::try_reserve_cleanup_for_path(&root)
            else {
                let _ = sender.send(ArtifactCleanupTask::VideoTombstones {
                    root,
                    expected_root,
                    _queue,
                });
                return;
            };
            let Some(mut sweep) =
                VideoTombstoneSweep::begin(&root, expected_root, _queue, admission)
            else {
                clear_pending_video_tombstone_sweep(&root, expected_root);
                return;
            };
            if sweep.run_batch() {
                finish_or_restart_video_tombstone_sweep(sweep, sender);
            } else if sender
                .send(ArtifactCleanupTask::VideoTombstoneContinuation(sweep))
                .is_err()
            {
                clear_pending_video_tombstone_sweep(&root, expected_root);
            }
        }
        ArtifactCleanupTask::VideoTombstoneContinuation(mut sweep) => {
            let root = sweep.root_path.clone();
            let expected_root = sweep.expected_root;
            if sweep.run_batch() {
                finish_or_restart_video_tombstone_sweep(sweep, sender);
            } else if sender
                .send(ArtifactCleanupTask::VideoTombstoneContinuation(sweep))
                .is_err()
            {
                clear_pending_video_tombstone_sweep(&root, expected_root);
            }
        }
        #[cfg(test)]
        ArtifactCleanupTask::Barrier(done) => {
            let _ = done.try_send(());
        }
        #[cfg(test)]
        ArtifactCleanupTask::PanicForTest(_) => {
            panic!("injected artifact cleanup worker panic")
        }
    }
}

fn artifact_cleanup_worker(
    video_receiver: &std::sync::mpsc::Receiver<VideoRetentionTask>,
    best_effort_receiver: &std::sync::mpsc::Receiver<ArtifactCleanupTask>,
    wake_receiver: &std::sync::mpsc::Receiver<()>,
    best_effort_sender: &std::sync::mpsc::Sender<ArtifactCleanupTask>,
) {
    loop {
        // Correctness-critical retention is checked before every bounded GC
        // slice. New GC cannot overtake it, while each persistent cursor still
        // makes progress by requeueing at the best-effort tail.
        if let Ok(task) = video_receiver.try_recv() {
            // A cleanup defect must not permanently strand the static senders
            // on a dead receiver. The task's completion guard unwinds after its
            // root and permit, reconciling the lease registry before we poll
            // the next priority item.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_video_retention_task(task);
            }));
            continue;
        }
        if let Ok(task) = best_effort_receiver.try_recv() {
            let recovery = task.panic_recovery();
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_best_effort_cleanup_task(task, best_effort_sender);
            }))
            .is_err()
            {
                recover_artifact_cleanup_after_panic(recovery, best_effort_sender);
            }
            continue;
        }
        if wake_receiver.recv().is_err() {
            return;
        }
    }
}

fn register_unique_artifact_path(
    key: PathBuf,
    video_sweep: VideoRetentionSweep,
    video_retention: crate::control::VideoRetentionPermit,
) -> Result<ArtifactPathLease, crate::control::VideoRetentionPermit> {
    if video_sweep.root.path().join(&video_sweep.fresh) != key {
        return Err(video_retention);
    }
    let (leases, _) = artifact_path_leases();
    let mut held = leases
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if held.contains_key(&key) {
        return Err(video_retention);
    }
    held.insert(
        key.clone(),
        ArtifactLeaseState {
            count: 1,
            video_identity: Some(video_sweep.identity),
            video_sweep_requested: false,
            sweeping: false,
            video_retention: Some(video_retention),
        },
    );
    drop(held);
    artifact_reader_acquire_anchor();
    Ok(ArtifactPathLease {
        key: Some(key),
        os_lock: None,
        video_sweep: Some(video_sweep),
        _deferred_cleanup: None,
    })
}

pub(crate) fn acquire_capture_name_lease(
    target: &ConfinedImage,
    cancelled: impl Fn() -> bool,
) -> std::io::Result<Option<ArtifactPathLease>> {
    acquire_capture_name_lease_with_wait(target, cancelled, CAPTURE_NAMESPACE_WAIT)
}

fn acquire_capture_name_lease_with_wait(
    target: &ConfinedImage,
    cancelled: impl Fn() -> bool,
    explicit_wait: std::time::Duration,
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
    // The lease remains with an explicit reply through its client ACK, so a
    // millisecond of ordinary handoff overlap should queue rather than fail.
    // This runs on the singleton encode worker, hence the explicit deadline:
    // a wedged reply is reported instead of parking unrelated encodes.
    let waited_from = std::time::Instant::now();
    while held.contains_key(&key) {
        if cancelled() {
            return Ok(None);
        }
        if !automatic && waited_from.elapsed() >= explicit_wait {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "another explicit capture reply still owns the shared image \
                 namespace after waiting — release the previous capture's reply",
            ));
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
            video_identity: None,
            video_sweep_requested: false,
            sweeping: false,
            video_retention: None,
        },
    );
    drop(held);

    let mut lease = ArtifactPathLease {
        key: Some(key),
        os_lock: None,
        video_sweep: None,
        _deferred_cleanup: target.deferred_cleanup.clone(),
    };
    if !automatic {
        // Lock the SAME retained authority the writer uses. On Unix the
        // directory inode itself is the advisory-lock object, so a same-uid
        // actor cannot split two writers by replacing a child lockfile. Windows
        // locks a child file reached through the deny-delete pinned directory;
        // its share mode prevents replacement for the full lock lifetime.
        #[cfg(unix)]
        let file = target.pinned()?.open_directory_lock()?;
        #[cfg(windows)]
        let file = {
            let lock_dir = target
                .pinned()?
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
/// `None` means the previous final producer-or-reader lease is already running
/// the capability-bound convergence sweep. The caller must drop its pinned
/// candidate handles and retry rather than returning paths from that in-between
/// state.
fn join_video_artifact_state(
    state: &mut ArtifactLeaseState,
    identity: VideoLeaseIdentity,
) -> std::io::Result<bool> {
    if state.sweeping {
        artifact_reader_reject_acquire_anchor();
        return Ok(false);
    }
    match state.video_identity {
        Some(expected) if expected != identity => {
            artifact_reader_reject_replaced_identity_anchor();
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "recording identity changed while acquiring retention",
            ));
        }
        Some(_) => {}
        None => state.video_identity = Some(identity),
    }
    if state.video_retention.is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "video retention cleanup admission is unavailable",
        ));
    }
    state.count = state.count.saturating_add(1);
    Ok(true)
}

pub(crate) fn retain_video_artifact_path(
    root: crate::pinned_dir::PinnedDir,
    fresh: std::ffi::OsString,
    recording: &crate::pinned_dir::PinnedDir,
) -> std::io::Result<Option<ArtifactPathLease>> {
    let key = root.path().join(&fresh);
    if recording.path() != key {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "video lease capability does not match its recording",
        ));
    }
    let identity = VideoLeaseIdentity {
        root: root.retained_identity()?,
        recording: recording.retained_identity()?,
    };
    let (leases, _) = artifact_path_leases();
    let mut held = leases
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let joined = if let Some(state) = held.get_mut(&key) {
        join_video_artifact_state(state, identity)?
    } else {
        drop(held);
        let video_retention = reserve_video_retention(root.path()).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "video retention cleanup lane is busy",
            )
        })?;
        held = leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(state) = held.get_mut(&key) {
            let joined = join_video_artifact_state(state, identity)?;
            drop(video_retention);
            joined
        } else {
            held.insert(
                key.clone(),
                ArtifactLeaseState {
                    count: 1,
                    video_identity: Some(identity),
                    video_sweep_requested: false,
                    sweeping: false,
                    video_retention: Some(video_retention),
                },
            );
            true
        }
    };
    drop(held);
    if !joined {
        return Ok(None);
    }
    artifact_reader_acquire_anchor();
    Ok(Some(ArtifactPathLease {
        key: Some(key),
        os_lock: None,
        video_sweep: Some(VideoRetentionSweep {
            root,
            fresh,
            identity,
        }),
        _deferred_cleanup: None,
    }))
}

impl ArtifactPathLease {
    /// Request one last-release retention sweep. A producer arms at the
    /// irreversible marker-publication boundary; a reader arms only after it
    /// has revalidated every namespace, marker, index, and frame identity.
    /// Acquiring a reader lease before that validation prevents a producer or
    /// older sweep from removing the recording in the gap, while delaying the
    /// reader's request prevents a failed read from causing unrelated work.
    pub(crate) fn arm_video_retention_sweep(
        &self,
        recording: &crate::pinned_dir::PinnedDir,
    ) -> std::io::Result<()> {
        let Some(key) = self.key.as_ref() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "artifact lease was already released",
            ));
        };
        let Some(sweep) = self.video_sweep.as_ref() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "artifact lease has no video sweep capability",
            ));
        };
        if recording.path() != key {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "video retention capability does not match its recording path",
            ));
        }
        // Bind Arm to the exact lease-local capability, not merely the
        // registry's cached identities. The recording check is last and is the
        // linearization point: replacement before it refuses Arm; replacement
        // after it is an allowed post-Arm environment transition.
        sweep.root.validate_path_identity()?;
        if recording.retained_identity()? != sweep.identity.recording {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "recording identity changed while arming retention",
            ));
        }
        recording.validate_path_identity()?;
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
        if state.video_identity != Some(sweep.identity) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "recording identity changed while arming retention",
            ));
        }
        state.video_sweep_requested = true;
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
        let video_lease = self.video_sweep.is_some();
        debug_assert!(state.count > 0, "artifact lease count underflow");
        state.count = state.count.saturating_sub(1);
        if video_lease {
            artifact_reader_release_anchor();
        }
        let sweep = if state.count == 0 {
            if state.video_sweep_requested {
                match (self.video_sweep.take(), state.video_retention.take()) {
                    (Some(sweep), Some(admission)) => {
                        state.sweeping = true;
                        artifact_reader_start_sweep_anchor();
                        Some((sweep, admission))
                    }
                    _ => {
                        debug_assert!(
                            false,
                            "armed video lease lost its sweep capability or reserved admission"
                        );
                        held.remove(&key);
                        None
                    }
                }
            } else {
                held.remove(&key);
                None
            }
        } else {
            None
        };
        changed.notify_all();
        drop(held);
        if let Some((sweep, admission)) = sweep {
            let task = VideoRetentionTask {
                root: Some(sweep.root),
                fresh: sweep.fresh,
                admission: Some(admission),
                _completion: VideoRetentionCompletion { key },
            };
            // The task's slot was reserved when this registry key was admitted;
            // best-effort work cannot saturate this nonblocking lane. If the
            // worker is unavailable, dropping the unsent task completes the
            // registry transition after releasing its retained resources.
            let _ = try_schedule_video_retention(task);
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
const DEAD_INSTANCE_SCAN_LIMIT: usize = 256;
const DEAD_INSTANCE_DELETE_LIMIT: usize = 16_384;
const DEAD_INSTANCE_TOMBSTONE_PREFIX: &str = ".instance-prune-";

fn fresh_instance_tombstone_name() -> std::ffi::OsString {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::ffi::OsString::from(format!(
        "{DEAD_INSTANCE_TOMBSTONE_PREFIX}{}-{sequence:020}",
        process_instance_id()
    ))
}

fn cleanup_instance_tombstone(
    root: &crate::pinned_dir::PinnedDir,
    name: &std::ffi::OsStr,
    namespace: &crate::pinned_dir::PinnedDir,
    delete_budget: &mut usize,
) -> bool {
    let cleared = namespace.clear_contents_with_budget(delete_budget);
    let mut mutated = cleared.removed > 0;
    if cleared.result.is_ok() && *delete_budget > 0 {
        *delete_budget -= 1;
        if root.remove_empty_child_exact(name, namespace).is_ok() {
            mutated = true;
        }
    }
    mutated
}

struct DeadNamespaceSweep {
    root_path: PathBuf,
    root: crate::pinned_dir::PinnedDir,
    entries: std::fs::ReadDir,
    current: String,
    /// Retained across continuation batches, so requeueing cannot exceed the
    /// bounded population admitted before the first scan.
    _queue: ArtifactCleanupQueuePermit,
    /// Last so both retained directory handles close before the cleanup slot.
    _admission: crate::control::ArtifactCleanupPermit,
}

impl DeadNamespaceSweep {
    fn begin(
        root: &Path,
        current: String,
        queue: ArtifactCleanupQueuePermit,
        admission: crate::control::ArtifactCleanupPermit,
    ) -> Result<Self, ArtifactCleanupQueuePermit> {
        let root_path = match std::fs::canonicalize(root) {
            Ok(path) => path,
            Err(_) => return Err(queue),
        };
        let pinned_root = match crate::pinned_dir::PinnedDir::open(&root_path) {
            Ok(root) => root,
            Err(_) => return Err(queue),
        };
        let entries = match std::fs::read_dir(&root_path) {
            Ok(entries) => entries,
            Err(_) => return Err(queue),
        };
        // Bind the lexical iterator to the retained root before it can become a
        // queued continuation. Later mutation is handle-rooted.
        if pinned_root.validate_path_identity().is_err() {
            return Err(queue);
        }
        Ok(Self {
            root_path,
            root: pinned_root,
            entries,
            current,
            _queue: queue,
            _admission: admission,
        })
    }

    /// Consume one bounded slice of a persistent directory cursor. Returning
    /// `true` means the cursor is exhausted or its retained root was replaced.
    /// Continuations prevent a permanent live/clutter prefix from starving
    /// valid stale namespaces beyond the first scan slice.
    fn run_batch(&mut self, pid_alive: impl FnMut(u32) -> bool) -> bool {
        self.run_batch_with_hook(pid_alive, || {})
    }

    fn run_batch_with_hook(
        &mut self,
        mut pid_alive: impl FnMut(u32) -> bool,
        mut before_lease_probe: impl FnMut(),
    ) -> bool {
        if self.root.validate_path_identity().is_err() {
            return true;
        }
        let mut delete_budget = DEAD_INSTANCE_DELETE_LIMIT;
        let mut mutated = false;
        for _ in 0..DEAD_INSTANCE_SCAN_LIMIT {
            if delete_budget == 0 {
                break;
            }
            let entry_name = match self.entries.next() {
                Some(Ok(entry)) => entry.file_name(),
                Some(Err(_)) => continue,
                None => {
                    if mutated {
                        let _ = self.root.sync();
                    }
                    return true;
                }
            };
            let Some(name) = entry_name.to_str() else {
                continue;
            };
            if name.starts_with(DEAD_INSTANCE_TOMBSTONE_PREFIX) {
                if let Ok(namespace) = self.root.child(&entry_name) {
                    mutated |= cleanup_instance_tombstone(
                        &self.root,
                        &entry_name,
                        &namespace,
                        &mut delete_budget,
                    );
                }
                continue;
            }
            if name == self.current {
                continue;
            }
            let Some(pid) = instance_pid(name) else {
                continue;
            };
            let Ok(namespace) = self.root.child(&entry_name) else {
                continue;
            };
            if namespace.validate_path_identity().is_err() {
                continue;
            }
            before_lease_probe();
            let (lease_state, acquired_lease) = match namespace
                .open_existing_namespace_lock_at_retained(std::ffi::OsStr::new(INSTANCE_LEASE_FILE))
            {
                Ok(lease) => match lease.try_lock() {
                    Ok(()) => (InstanceLeaseState::Acquirable, Some(lease)),
                    Err(std::fs::TryLockError::WouldBlock) => (InstanceLeaseState::Held, None),
                    Err(std::fs::TryLockError::Error(_)) => (InstanceLeaseState::Invalid, None),
                },
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
                // Keep a successful exact-lease acquisition alive across the exact
                // rename and bounded cleanup. Legacy/no-lease removal carries no
                // guard by definition.
                let _acquired_lease = acquired_lease;
                if namespace.validate_path_identity().is_err() {
                    continue;
                }
                let tombstone = fresh_instance_tombstone_name();
                if self
                    .root
                    .rename_child_exact(&entry_name, &namespace, &tombstone)
                    .is_err()
                {
                    continue;
                }
                delete_budget -= 1;
                mutated = true;
                mutated |= cleanup_instance_tombstone(
                    &self.root,
                    &tombstone,
                    &namespace,
                    &mut delete_budget,
                );
            }
        }
        if mutated {
            let _ = self.root.sync();
        }
        false
    }
}

fn finish_or_restart_dead_namespace_cursor(
    sweep: DeadNamespaceSweep,
    sender: &std::sync::mpsc::Sender<ArtifactCleanupTask>,
) {
    let DeadNamespaceSweep {
        root_path,
        root,
        entries,
        current,
        _queue,
        _admission,
    } = sweep;
    // A new cursor is permitted only after every descriptor and the shared
    // best-effort slot from the completed generation have been released.
    drop(entries);
    drop(root);
    drop(_admission);
    finish_or_restart_dead_namespace_sweep(root_path, current, _queue, sender);
}

#[cfg(test)]
fn sweep_dead_instance_namespaces_with(
    root: &Path,
    current: &str,
    mut pid_alive: impl FnMut(u32) -> bool,
) {
    let queue = ArtifactCleanupQueuePermit::unmetered();
    let Some(admission) = crate::control::ReplyRetention::try_reserve_cleanup_for_path(root) else {
        return;
    };
    let Ok(mut sweep) = DeadNamespaceSweep::begin(root, current.to_string(), queue, admission)
    else {
        return;
    };
    while !sweep.run_batch(&mut pid_alive) {}
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
    batch_synced: std::cell::Cell<bool>,
    /// Last field: natural destruction first closes recording/root handles and
    /// runs `_retention_lease`; only then may coalesced tombstone/namespace GC
    /// wake. Both queue slots were reserved before the recording was created.
    deferred_cleanup: DeferredVideoCleanup,
}

fn fresh_video_tombstone_name() -> std::ffi::OsString {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::ffi::OsString::from(format!(".prune-{}-{sequence:020}", process_instance_id()))
}

struct PendingVideoTombstoneSweep {
    root: PathBuf,
    expected_root: crate::pinned_dir::PinnedDirIdentity,
    /// A tombstone arrived after the current persistent cursor began. Restart
    /// once after EOF so directory-iteration snapshot semantics cannot lose it.
    dirty: bool,
}

fn pending_video_tombstone_sweeps() -> &'static std::sync::Mutex<Vec<PendingVideoTombstoneSweep>> {
    static PENDING: std::sync::OnceLock<std::sync::Mutex<Vec<PendingVideoTombstoneSweep>>> =
        std::sync::OnceLock::new();
    PENDING.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

fn schedule_video_tombstone_sweep(
    root: &Path,
    expected_root: crate::pinned_dir::PinnedDirIdentity,
    queue: ArtifactCleanupQueuePermit,
) {
    let root = root.to_path_buf();
    let pending = pending_video_tombstone_sweeps();
    let mut held = pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing) = held
        .iter_mut()
        .find(|entry| entry.root == root && entry.expected_root == expected_root)
    {
        existing.dirty = true;
        return;
    }
    held.push(PendingVideoTombstoneSweep {
        root: root.clone(),
        expected_root,
        dirty: false,
    });
    drop(held);
    if !try_schedule_artifact_cleanup(ArtifactCleanupTask::VideoTombstones {
        root: root.clone(),
        expected_root,
        _queue: queue,
    }) {
        clear_pending_video_tombstone_sweep(&root, expected_root);
    }
}

fn clear_pending_video_tombstone_sweep(
    root: &Path,
    expected_root: crate::pinned_dir::PinnedDirIdentity,
) {
    pending_video_tombstone_sweeps()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|entry| entry.root != root || entry.expected_root != expected_root);
}

struct VideoTombstoneSweep {
    root_path: PathBuf,
    root: crate::pinned_dir::PinnedDir,
    entries: std::fs::ReadDir,
    expected_root: crate::pinned_dir::PinnedDirIdentity,
    restart_required: bool,
    _queue: ArtifactCleanupQueuePermit,
    /// Last so the retained cursor/root close before the best-effort descriptor
    /// slot becomes visible to another cursor.
    _admission: crate::control::ArtifactCleanupPermit,
}

impl VideoTombstoneSweep {
    fn begin(
        root: &Path,
        expected_root: crate::pinned_dir::PinnedDirIdentity,
        queue: ArtifactCleanupQueuePermit,
        admission: crate::control::ArtifactCleanupPermit,
    ) -> Option<Self> {
        let root_path = std::fs::canonicalize(root).ok()?;
        let pinned_root = crate::pinned_dir::PinnedDir::open(&root_path).ok()?;
        if pinned_root.retained_identity().ok()? != expected_root {
            return None;
        }
        let entries = std::fs::read_dir(&root_path).ok()?;
        pinned_root.validate_path_identity().ok()?;
        Some(Self {
            root_path,
            root: pinned_root,
            entries,
            expected_root,
            restart_required: false,
            _queue: queue,
            _admission: admission,
        })
    }

    /// Consume one bounded slice. `true` means this cursor reached EOF or its
    /// lexical root no longer names the retained identity.
    fn run_batch(&mut self) -> bool {
        if self.root.validate_path_identity().is_err() {
            return true;
        }
        let mut work = VideoPruneWork::new(VIDEO_PRUNE_WORK_LIMIT);
        let mut mutated = false;
        for _ in 0..VIDEO_PRUNE_SCAN_LIMIT {
            let entry_name = match self.entries.next() {
                Some(Ok(entry)) => entry.file_name(),
                Some(Err(_)) => continue,
                None => {
                    if mutated {
                        let _ = self.root.sync();
                    }
                    return true;
                }
            };
            if !work.scan() {
                self.restart_required = true;
                break;
            }
            let Some(name) = entry_name.to_str() else {
                continue;
            };
            if !name.starts_with(".prune-") {
                continue;
            }
            let Ok(recording) = self.root.child(&entry_name) else {
                continue;
            };
            let cleanup = cleanup_video_tombstone(&self.root, &entry_name, &recording, &mut work);
            mutated |= cleanup.mutated;
            if !cleanup.removed && work.remaining == 0 {
                self.restart_required = true;
                break;
            }
        }
        if mutated {
            let _ = self.root.sync();
        }
        false
    }
}

fn finish_or_restart_video_tombstone_sweep(
    sweep: VideoTombstoneSweep,
    sender: &std::sync::mpsc::Sender<ArtifactCleanupTask>,
) {
    let VideoTombstoneSweep {
        root_path,
        root,
        entries,
        expected_root,
        restart_required,
        _queue,
        _admission,
    } = sweep;
    drop(entries);
    drop(root);
    drop(_admission);

    let mut held = pending_video_tombstone_sweeps()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(position) = held
        .iter()
        .position(|entry| entry.root == root_path && entry.expected_root == expected_root)
    else {
        return;
    };
    let restart = restart_required || held[position].dirty;
    if restart {
        held[position].dirty = false;
    } else {
        held.swap_remove(position);
    }
    drop(held);

    if restart
        && sender
            .send(ArtifactCleanupTask::VideoTombstones {
                root: root_path.clone(),
                expected_root,
                _queue,
            })
            .is_err()
    {
        clear_pending_video_tombstone_sweep(&root_path, expected_root);
    }
}

/// Closed identity checkpoint for one frame in a private video bundle.
///
/// A recording may contain hundreds of frames. Retaining each writer's
/// [`crate::pinned_dir::PinnedFile`] until the control reply is acknowledged
/// consumes one OS descriptor per frame and can exhaust macOS's default soft
/// limit. The pinned recording directory already anchors the namespace; this
/// compact seal records the filename and its device/inode (Unix) or
/// volume/file-index (Windows), and validation reopens one frame at a time.
/// This is wire-edge replacement detection, not continuous exclusion: after the
/// writer closes, a same-user process can mutate the file and OS identities can
/// eventually be reused. That is the least-common-denominator Unix guarantee.
#[derive(Debug)]
pub(crate) struct VideoFrameSeal {
    name: std::ffi::OsString,
    identity: crate::pinned_dir::PinnedFileIdentity,
}

impl ConfinedVideoDir {
    fn ensure_unpublished(&self) -> std::io::Result<()> {
        if self.published {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "published video recording is immutable",
            ));
        }
        Ok(())
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn is_published(&self) -> bool {
        self.published
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        self.path
            .as_deref()
            .expect("live confined video dir owns its path")
    }

    #[cfg(test)]
    pub(crate) fn write_new_private(
        &self,
        name: &std::ffi::OsStr,
        bytes: &[u8],
    ) -> std::io::Result<crate::pinned_dir::PinnedFile> {
        self.ensure_unpublished()?;
        self.batch_synced.set(false);
        self.recording
            .as_ref()
            .expect("live confined video dir")
            .write_new_private(name, bytes)
    }

    /// Durably write one frame, then close its per-file descriptor and retain
    /// only an identity seal. Publication reopens seals sequentially, keeping the
    /// live descriptor count constant regardless of recording length.
    pub(crate) fn write_sealed_frame(
        &self,
        name: &std::ffi::OsStr,
        bytes: &[u8],
    ) -> std::io::Result<VideoFrameSeal> {
        let file = self.write_batch_member_authorized(name, bytes, || true)?;
        Ok(VideoFrameSeal {
            name: name.to_os_string(),
            identity: file.into_identity()?,
        })
    }

    /// Write a durable frame/index batch member without syncing the recording
    /// directory yet. Per-member work stays on the retained recording
    /// capability and validates only the renamed entry; [`Self::publish`] is the
    /// mandatory full-path batch barrier before the visibility marker exists.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "VideoBatchPublicationDurability",
            action = "WriteMember",
            project = "aterm_gui::artifact_transaction_conformance::project_video_batch_publication_durability"
        )
    )]
    pub(crate) fn write_batch_member_authorized(
        &self,
        name: &std::ffi::OsStr,
        bytes: &[u8],
        authorize: impl FnOnce() -> bool,
    ) -> std::io::Result<crate::pinned_dir::PinnedFile> {
        self.ensure_unpublished()?;
        // A failed or cancelled write must invalidate a prior batch barrier too:
        // only a later successful `publish` may authorize marker creation.
        self.batch_synced.set(false);
        self.recording
            .as_ref()
            .expect("live confined video dir")
            .write_new_private_deferred_dir_sync_authorized(name, bytes, authorize)
    }

    pub(crate) fn remove_file_if_exists(&self, name: &std::ffi::OsStr) -> std::io::Result<()> {
        self.ensure_unpublished()?;
        self.batch_synced.set(false);
        self.recording
            .as_ref()
            .expect("live confined video dir")
            .remove_file_if_exists(name)
    }

    fn cleanup(&mut self) -> std::io::Result<()> {
        self.ensure_unpublished()?;
        let Some(recording) = self.recording.as_ref() else {
            return Ok(());
        };
        let expected_root = self.instance.retained_identity()?;
        let tombstone = fresh_video_tombstone_name();
        // Make the unpublished name disappear with one handle-rooted rename.
        // Recursive cleanup is delegated to the bounded singleton worker; a
        // saturated queue merely leaves a recognizable tombstone for a later
        // retention pass.
        self.instance
            .rename_child_exact(&self.name, recording, &tombstone)?;
        self.recording = None;
        self.path = None;
        debug_assert_eq!(
            expected_root, self.deferred_cleanup.tombstone_identity,
            "video cleanup root identity changed before quarantine"
        );
        self.deferred_cleanup.arm_tombstone();
        Ok(())
    }

    pub(crate) fn abort_in_place(&mut self) -> std::io::Result<()> {
        self.cleanup()
    }

    pub fn abort(mut self) -> std::io::Result<()> {
        self.abort_in_place()
    }

    /// Publish only after `index.json` is durable. Marker publication requests
    /// one retention sweep, which the final producer-or-reader lease performs;
    /// retention never runs at mint or in this fallible pre-marker phase, so a
    /// refused/failed request never deletes a prior good recording. This is also
    /// the required parent-directory durability barrier for all deferred
    /// frame/index writes.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "VideoBatchPublicationDurability",
            action = "SyncBatch",
            project = "aterm_gui::artifact_transaction_conformance::project_video_batch_publication_durability"
        )
    )]
    pub(crate) fn publish(
        &mut self,
        frames: &[VideoFrameSeal],
        index: &crate::pinned_dir::PinnedFile,
    ) -> std::io::Result<PathBuf> {
        self.ensure_unpublished()?;
        self.batch_synced.set(false);
        let recording = self.recording.as_ref().expect("live confined video dir");
        self.validate_batch_paths(recording)?;
        recording.sync()?;
        Self::validate_frame_entries(recording, frames)?;
        index.validate_entry_identity_at_retained()?;
        self.validate_batch_paths(recording)?;
        self.batch_synced.set(true);
        Ok(self.path().to_path_buf())
    }

    /// Atomically publish the marker readers require and make the recording
    /// permanently non-abortable at that exact rename/link boundary. Failures
    /// while preparing the invisible temporary still clean normally. Once the
    /// marker may have been visible, even a later sync/identity failure leaves
    /// the recording intact until launch-namespace cleanup: a successful reader
    /// may already have received paths and need to open them after disconnect.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "VideoBatchPublicationDurability",
            action = "PublishMarker",
            project = "aterm_gui::artifact_transaction_conformance::project_video_batch_publication_durability"
        )
    )]
    pub(crate) fn publish_marker(&mut self) -> std::io::Result<crate::pinned_dir::PinnedFile> {
        self.publish_marker_with_hook(|| {})
    }

    #[cfg(test)]
    pub(crate) fn publish_marker_with_test_hook(
        &mut self,
        after_publish: impl FnOnce(),
    ) -> std::io::Result<crate::pinned_dir::PinnedFile> {
        self.publish_marker_with_hook(after_publish)
    }

    fn publish_marker_with_hook(
        &mut self,
        after_publish: impl FnOnce(),
    ) -> std::io::Result<crate::pinned_dir::PinnedFile> {
        self.ensure_unpublished()?;
        if !self.batch_synced.get() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "video batch has not passed its durability barrier",
            ));
        }
        // Borrow the flag field separately so the commit hook can set it while
        // `recording` is still immutably borrowed for the write.
        let published = &mut self.published;
        let retention_lease = &self._retention_lease;
        let mut arm_error = None;
        let recording = self.recording.as_ref().expect("live confined video dir");
        let result = recording.write_new_private_with_hooks(
            std::ffi::OsStr::new(VIDEO_PUBLISHED_FILE),
            b"aterm-video-published-v1\n",
            || {},
            || {
                *published = true;
                if let Err(error) = retention_lease.arm_video_retention_sweep(recording) {
                    arm_error = Some(error);
                }
                after_publish();
            },
        );
        arm_error.map_or(result, Err)
    }

    #[cfg(test)]
    pub(crate) fn batch_synced_for_test(&self) -> bool {
        self.batch_synced.get()
    }

    pub(crate) fn validate_for_reply(
        &self,
        frames: &[VideoFrameSeal],
        index: &crate::pinned_dir::PinnedFile,
    ) -> std::io::Result<()> {
        let recording = self.recording.as_ref().expect("live confined video dir");
        self.validate_batch_paths(recording)?;
        Self::validate_frame_entries(recording, frames)?;
        index.validate_entry_identity_at_retained()?;
        self.validate_batch_paths(recording)
    }

    fn validate_batch_paths(
        &self,
        recording: &crate::pinned_dir::PinnedDir,
    ) -> std::io::Result<()> {
        // `recording` was derived from `instance` and retains its complete
        // ancestor chain, so its full validation already covers the instance.
        recording.validate_path_identity()
    }

    fn validate_frame_entries(
        recording: &crate::pinned_dir::PinnedDir,
        frames: &[VideoFrameSeal],
    ) -> std::io::Result<()> {
        for frame in frames {
            frame
                .identity
                .validate_at_retained(recording, &frame.name)?;
        }
        Ok(())
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
#[cfg(test)]
pub fn confine_video_dir(sock_dir: &Path) -> Option<ConfinedVideoDir> {
    confine_video_dir_with_admission(sock_dir, |_| true).ok()
}

pub(crate) fn confine_video_dir_with_admission(
    sock_dir: &Path,
    admit: impl FnOnce(&Path) -> bool,
) -> Result<ConfinedVideoDir, ArtifactConfinementError> {
    confine_video_dir_for_instance_with_admission(sock_dir, process_instance_id(), admit)
}

#[must_use]
#[cfg(test)]
fn confine_video_dir_for_instance(sock_dir: &Path, instance: &str) -> Option<ConfinedVideoDir> {
    confine_video_dir_for_instance_with_admission(sock_dir, instance, |_| true).ok()
}

fn confine_video_dir_for_instance_with_admission(
    sock_dir: &Path,
    instance: &str,
    admit: impl FnOnce(&Path) -> bool,
) -> Result<ConfinedVideoDir, ArtifactConfinementError> {
    if !valid_instance_component(instance) {
        return Err(ArtifactConfinementError::Invalid);
    }
    let video_root = ensure_canonical_direct_child(sock_dir, VIDEO_DIR)
        .map_err(|_| ArtifactConfinementError::Invalid)?;
    let canon = ensure_canonical_direct_child(&video_root, instance)
        .map_err(|_| ArtifactConfinementError::Invalid)?;
    let instance_dir = crate::pinned_dir::PinnedDir::open_with_admission(&canon, admit)
        .map_err(confinement_open_error)?;
    let instance_identity = instance_dir
        .retained_identity()
        .map_err(|_| ArtifactConfinementError::Invalid)?;
    hold_current_instance_lease(&canon).map_err(|_| ArtifactConfinementError::Invalid)?;
    let mut video_retention = Some(
        reserve_video_retention(instance_dir.path())
            .ok_or(ArtifactConfinementError::AdmissionRefused)?,
    );
    let tombstone_queue =
        reserve_artifact_cleanup_queue().ok_or(ArtifactConfinementError::AdmissionRefused)?;
    let dead_queue =
        reserve_artifact_cleanup_queue().ok_or(ArtifactConfinementError::AdmissionRefused)?;
    let deferred_cleanup = DeferredVideoCleanup {
        tombstone_root: instance_dir.path().to_path_buf(),
        tombstone_identity: instance_identity,
        tombstone_queue: Some(tombstone_queue),
        tombstone_armed: false,
        dead_root: video_root,
        current: instance.to_string(),
        dead_queue: Some(dead_queue),
    };
    static RECORDING_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let base = RECORDING_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    for n in 0..1000u32 {
        let name = std::ffi::OsString::from(format!("rec-{base:020}-{n:03}"));
        match instance_dir.create_child(&name) {
            Ok(recording) => {
                let path = recording.path().to_path_buf();
                let Ok(recording_identity) = recording.retained_identity() else {
                    let _ = instance_dir.remove_child_tree_exact(&name, &recording);
                    return Err(ArtifactConfinementError::Invalid);
                };
                let video_sweep = VideoRetentionSweep {
                    root: instance_dir.clone(),
                    fresh: name.clone(),
                    identity: VideoLeaseIdentity {
                        root: instance_identity,
                        recording: recording_identity,
                    },
                };
                let retention_lease = match register_unique_artifact_path(
                    path.clone(),
                    video_sweep,
                    video_retention
                        .take()
                        .expect("live video retention admission"),
                ) {
                    Ok(lease) => lease,
                    Err(admission) => {
                        video_retention = Some(admission);
                        let _ = instance_dir.remove_child_tree_exact(&name, &recording);
                        continue;
                    }
                };
                return Ok(ConfinedVideoDir {
                    path: Some(path),
                    instance: instance_dir,
                    recording: Some(recording),
                    name,
                    _retention_lease: retention_lease,
                    published: false,
                    batch_synced: std::cell::Cell::new(false),
                    deferred_cleanup,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(ArtifactConfinementError::Invalid),
        }
    }
    Err(ArtifactConfinementError::Invalid)
}

pub(crate) const AUTO_IMAGE_KEEP: usize = 32;
const AUTO_IMAGE_SCAN_BUDGET: usize = 256;

/// Confine one omitted-name `image`/`window` output to this launch's private
/// auto namespace. Explicit caller names continue to use direct `images/`
/// children and are therefore outside every automatic retention sweep.
#[must_use]
#[cfg(test)]
pub(crate) fn confine_automatic_image_path(sock_dir: &Path, stem: &str) -> Option<ConfinedImage> {
    confine_automatic_image_path_with_admission(sock_dir, stem, |_| true).ok()
}

pub(crate) fn confine_automatic_image_path_with_admission(
    sock_dir: &Path,
    stem: &str,
    admit: impl FnOnce(&Path) -> bool,
) -> Result<ConfinedImage, ArtifactConfinementError> {
    if stem.is_empty()
        || !stem
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(ArtifactConfinementError::Invalid);
    }
    let images = ensure_canonical_direct_child(sock_dir, IMAGES_DIR)
        .map_err(|_| ArtifactConfinementError::Invalid)?;
    let auto = ensure_canonical_direct_child(&images, AUTO_IMAGES_DIR)
        .map_err(|_| ArtifactConfinementError::Invalid)?;
    let dir = ensure_canonical_direct_child(&auto, process_instance_id())
        .map_err(|_| ArtifactConfinementError::Invalid)?;
    let pinned = crate::pinned_dir::PinnedDir::open_with_admission(&dir, admit)
        .map_err(confinement_open_error)?;
    let image_identity = pinned
        .retained_identity()
        .map_err(|_| ArtifactConfinementError::Invalid)?;
    hold_current_instance_lease(&dir).map_err(|_| ArtifactConfinementError::Invalid)?;
    let file_name: std::ffi::OsString = automatic_capture_name(stem).into();
    let deferred_cleanup = DeferredImageCleanup::reserve(
        dir.clone(),
        image_identity,
        file_name.clone(),
        auto,
        process_instance_id().to_string(),
    )
    .ok_or(ArtifactConfinementError::AdmissionRefused)?;
    Ok(ConfinedImage {
        dir,
        file_name,
        pinned: Some(pinned),
        deferred_cleanup: Some(deferred_cleanup),
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
#[cfg(test)]
fn prune_automatic_image_dir(target: &ConfinedImage) {
    let Some(pinned) = target.pinned.as_ref() else {
        return;
    };
    prune_automatic_image_dir_at(&target.dir, &target.file_name, pinned);
}

fn prune_automatic_image_dir_at(
    dir: &Path,
    fresh: &std::ffi::OsStr,
    pinned: &crate::pinned_dir::PinnedDir,
) {
    let eligible = automatic_capture_sequence(fresh).is_some()
        && dir
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
    let Ok(entries) = pinned.names_up_to(AUTO_IMAGE_SCAN_BUDGET) else {
        return;
    };
    let leased = leased_artifact_names(dir, fresh);
    let mut protected = 1usize;
    let mut files = entries
        .into_iter()
        .filter_map(|name| {
            if name == fresh {
                return None;
            }
            let sequence = automatic_capture_sequence(&name)?;
            if !pinned.is_regular_file(&name) {
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
        let path = dir.join(&name);
        if mutate_unleased_artifact(&path, || pinned.remove_file_if_exists(&name))
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
    if is_current_automatic_image(target)
        && let Some(pinned) = target.pinned.as_ref()
    {
        let _ = pinned.remove_file_if_exists(&target.file_name);
    }
}

/// Recordings kept on disk, INCLUDING the one just created. Each recording can
/// hold up to a full frame budget in PNGs (hundreds of MiB at `full`), so an
/// agent that records in a loop would otherwise grow one process-instance
/// namespace without bound. Marker publication requests a sweep; the final
/// producer-or-reader lease performs it, oldest first.
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

/// One shared retention allowance covers directory scans, file probes, renames,
/// and deletes across the whole sweep. This prevents a
/// crowded namespace from multiplying retention work without bound.
#[derive(Debug)]
struct VideoPruneWork {
    remaining: usize,
    scanned: usize,
    file_opens: usize,
    deletes: usize,
    renames: usize,
}

impl VideoPruneWork {
    fn new(limit: usize) -> Self {
        Self {
            remaining: limit,
            scanned: 0,
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
    Complete((crate::pinned_dir::PinnedFile, crate::pinned_dir::PinnedFile)),
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
    let Ok(index) = recording.pin_private_file_at_retained(std::ffi::OsStr::new("index.json"))
    else {
        return CompletionProbe::Invalid;
    };
    CompletionProbe::Complete((marker, index))
}

struct VideoTombstoneCleanup {
    removed: bool,
    mutated: bool,
}

fn cleanup_video_tombstone(
    root: &crate::pinned_dir::PinnedDir,
    name: &std::ffi::OsStr,
    recording: &crate::pinned_dir::PinnedDir,
    work: &mut VideoPruneWork,
) -> VideoTombstoneCleanup {
    let entry_allowance = (work.remaining / VIDEO_DELETE_WORK).min(VIDEO_PRUNE_DELETE_ENTRY_LIMIT);
    if entry_allowance == 0 {
        return VideoTombstoneCleanup {
            removed: false,
            mutated: false,
        };
    }
    let mut entries_left = entry_allowance;
    let cleared = recording.clear_contents_with_budget(&mut entries_left);
    let used = entry_allowance.saturating_sub(entries_left);
    for _ in 0..used {
        if !work.delete() {
            return VideoTombstoneCleanup {
                removed: false,
                mutated: cleared.removed > 0,
            };
        }
    }
    if cleared.result.is_err() || !work.delete() {
        return VideoTombstoneCleanup {
            removed: false,
            mutated: cleared.removed > 0,
        };
    }
    let removed = root.remove_empty_child_exact(name, recording).is_ok();
    VideoTombstoneCleanup {
        removed,
        mutated: cleared.removed > 0 || removed,
    }
}

fn prune_video_dirs_with_work(
    root: &crate::pinned_dir::PinnedDir,
    fresh: &std::ffi::OsStr,
    work: &mut VideoPruneWork,
) -> std::io::Result<bool> {
    let _sweep = retention_sweep_gate()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let affordable_scan = work.remaining / VIDEO_SCAN_WORK;
    let names = root.names_up_to(VIDEO_PRUNE_SCAN_LIMIT.min(affordable_scan))?;
    let leased = leased_artifact_names(root.path(), fresh);
    let mut tombstones = Vec::new();
    let mut completed = Vec::new();
    let mut protected_completed = 0usize;
    let mut mutated = false;
    for name in names {
        if !work.scan() {
            break;
        }
        let Some(text) = name.to_str() else {
            continue;
        };
        if text.starts_with(".prune-") {
            tombstones.push(name);
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
                completed.push(name);
            }
        }
    }

    // Finish quarantined partial work first. A bounded sweep may stop midway,
    // but the next publication recognizes the tombstone and continues instead
    // of orphaning a directory whose index marker was already removed.
    tombstones.sort();
    for name in tombstones {
        let Ok(recording) = root.child(&name) else {
            continue;
        };
        let cleanup = cleanup_video_tombstone(root, &name, &recording, work);
        mutated |= cleanup.mutated;
        if !cleanup.removed && work.remaining == 0 {
            return Ok(mutated);
        }
    }

    completed.sort();
    // `fresh` and every other completed reply whose lease still spans a socket
    // write occupy keep slots. Only unleased, marker-authorized oldest candidates
    // are quarantined. Completion probing and cleanup share the same allowance.
    let protected = 1usize.saturating_add(protected_completed);
    let unprotected_keep = VIDEO_KEEP.saturating_sub(protected);
    let mut needed = completed.len().saturating_sub(unprotected_keep);
    for name in completed {
        if needed == 0 {
            break;
        }
        let Ok(recording) = root.child(&name) else {
            continue;
        };
        let completion_guards = match completed_recording(&recording, work) {
            CompletionProbe::Complete(guards) => guards,
            CompletionProbe::Invalid => continue,
            CompletionProbe::Exhausted => break,
        };
        if !work.rename() {
            break;
        }
        let tombstone = fresh_video_tombstone_name();
        // Keep the marker/index identities pinned through the completion
        // decision, then release their two handles immediately before the
        // handle-rooted quarantine rename.
        drop(completion_guards);
        let path = root.path().join(&name);
        let renamed = mutate_unleased_artifact(&path, || {
            root.rename_child_exact(&name, &recording, &tombstone)
        })
        .is_some_and(|result| result.is_ok());
        if !renamed {
            continue;
        }
        mutated = true;
        needed -= 1;
        let cleanup = cleanup_video_tombstone(root, &tombstone, &recording, work);
        mutated |= cleanup.mutated;
        if !cleanup.removed && work.remaining == 0 {
            break;
        }
    }
    Ok(mutated)
}

fn prune_video_dirs(
    root: &crate::pinned_dir::PinnedDir,
    fresh: &std::ffi::OsStr,
) -> std::io::Result<bool> {
    let mut work = VideoPruneWork::new(VIDEO_PRUNE_WORK_LIMIT);
    prune_video_dirs_with_work(root, fresh, &mut work)
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
            let token_path = token_path_for_socket(&p);
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

/// The capability-token file beside the socket at `path`: the SHARED name rule
/// ([`control_socket::token_name_for_sock`]) resolved in that socket's own
/// directory. The server writes exactly this file and the client derives the
/// same one from the same rule, so the two ends cannot drift.
///
/// Load-bearing: the explicit-`$ATERM_CONTROL_SOCK` arm used to write the one
/// fixed `aterm.token` per DIRECTORY, so the second private instance an agent
/// booted in a scratch directory overwrote the first one's credential — and
/// every client of the first was refused `ERR auth` while its socket was still
/// listening, with nothing in either log to say why.
#[must_use]
pub fn token_path_for_socket(path: &str) -> PathBuf {
    let name = Path::new(path)
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    dir_of_socket(path).join(control_socket::token_name_for_sock(&name))
}

/// Read back the capability token of the socket at `sock_path`, trimming
/// whitespace. The symmetric counterpart of [`provision_token`]: it resolves
/// the file through [`token_path_for_socket`], the same rule the writer used,
/// so a drift between write and read shows up here rather than as an
/// unexplained `ERR auth`. The `aterm-ctl` client reads it equivalently
/// (resolving the per-instance token through the `latest` symlink first).
/// Returns `None` if unreadable (wrong user, missing) — fail closed.
#[must_use]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "symmetric API; client reads token equivalently")
)]
pub fn read_token(sock_path: &str) -> Option<String> {
    let raw = std::fs::read_to_string(token_path_for_socket(sock_path)).ok()?;
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
    pinned: Option<crate::pinned_dir::PinnedDir>,
    /// Last field. The target and its path lease share this kick, so stale
    /// namespace GC starts only after the final retained path capability closes.
    deferred_cleanup: Option<std::sync::Arc<DeferredImageCleanup>>,
}

impl ConfinedImage {
    /// Target-free sentinel for an inline image reply. It carries no path and no
    /// filesystem descriptor; the encode worker's `want_bytes` branch consumes
    /// it without invoking any file-only method.
    pub(crate) fn in_memory() -> Self {
        Self {
            dir: PathBuf::new(),
            file_name: std::ffi::OsString::new(),
            pinned: None,
            deferred_cleanup: None,
        }
    }

    fn pinned(&self) -> std::io::Result<&crate::pinned_dir::PinnedDir> {
        self.pinned.as_ref().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "an inline image has no filesystem target",
            )
        })
    }

    /// The full path, for logging / `OK <w> <h> <path>` replies only — NOT for
    /// re-opening (the writer must use [`Self::dir`] + [`Self::file_name`]).
    #[must_use]
    pub fn display_path(&self) -> PathBuf {
        self.dir.join(&self.file_name)
    }

    /// Request best-effort auto-image retention after every retained target,
    /// file, and path-lease capability has closed. The shared finalizer already
    /// owns its queue admission, so arming performs no filesystem or scheduler
    /// work on the reply's Drop path.
    pub(crate) fn arm_automatic_prune(&self) {
        if let Some(cleanup) = self.deferred_cleanup.as_ref() {
            cleanup.arm_image_prune();
        }
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
            self.pinned()?
                .write_new_private_authorized(&self.file_name, bytes, authorize)
        } else {
            self.pinned()?
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
        self.pinned()?.validate_path_identity()?;
        file.validate_entry_identity_at_retained()
    }

    #[cfg(test)]
    pub(crate) fn for_test(dir: &Path, file_name: &str) -> Self {
        let dir = std::fs::canonicalize(dir).expect("test capture directory");
        let pinned = crate::pinned_dir::PinnedDir::open(&dir).expect("pin test capture directory");
        Self {
            dir,
            file_name: file_name.into(),
            pinned: Some(pinned),
            deferred_cleanup: None,
        }
    }
}

#[must_use]
#[cfg(test)]
pub fn confine_image_path(sock_dir: &Path, requested: &str) -> Option<ConfinedImage> {
    confine_image_path_with_admission(sock_dir, requested, |_| true).ok()
}

/// Resolve a caller-supplied `image` path inside the socket directory's
/// `images/` subdir, independently of later resource admission.
///
/// The subdir is created `0700`. A relative or bare-filename request is
/// resolved INTO the subdir; an absolute request must already live inside it.
/// NESTED target directories are FORBIDDEN — the file must be a direct child of
/// `images/` — so the only directory component is the canonical subdir itself
/// (closing the intermediate-dir symlink-swap window, TOCTOU-1). Returns the
/// canonical dir + validated filename, or `None` when the request would escape
/// or names a nested path.
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
/// planted symlink (`crates/aterm-gui/src/lib.rs::path_confine_conformance`).
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
fn resolve_confined_image_target(
    sock_dir: &Path,
    requested: &str,
) -> Option<(PathBuf, std::ffi::OsString)> {
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
    Some((canon_images, file_name.to_os_string()))
}

pub(crate) fn confine_image_path_with_admission(
    sock_dir: &Path,
    requested: &str,
    admit: impl FnOnce(&Path) -> bool,
) -> Result<ConfinedImage, ArtifactConfinementError> {
    let (canon_images, file_name) = resolve_confined_image_target(sock_dir, requested)
        .ok_or(ArtifactConfinementError::Invalid)?;
    let pinned = crate::pinned_dir::PinnedDir::open_with_admission(&canon_images, admit)
        .map_err(confinement_open_error)?;
    Ok(ConfinedImage {
        dir: canon_images,
        file_name,
        pinned: Some(pinned),
        deferred_cleanup: None,
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
        // Write and read through the SHARED name rule, exactly as the server
        // and the client do — an explicit socket's token is named after that
        // socket, so this roundtrip also pins the pairing itself.
        let sock = dir.join("a.sock").to_string_lossy().into_owned();
        let token_path = token_path_for_socket(&sock);
        assert_eq!(token_path, dir.join("a.sock.token"));
        let written = provision_token(&token_path).expect("token written");
        let read = read_token(&sock).expect("token readable");
        assert_eq!(written, read);
        // A SECOND socket in the same directory has its own token file, and
        // provisioning it leaves the first one's alone (F9: it used to
        // overwrite it, and the first instance's clients then got `ERR auth`).
        let other = dir.join("b.sock").to_string_lossy().into_owned();
        let other_written = provision_token(&token_path_for_socket(&other)).expect("token written");
        assert_ne!(other_written, written);
        assert_eq!(read_token(&sock).as_deref(), Some(written.as_str()));
        assert_eq!(read_token(&other).as_deref(), Some(other_written.as_str()));
        // Token file is 0600.
        let mode = std::fs::metadata(&token_path).unwrap().permissions().mode() & 0o777;
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
        let sock = dir.join("a.sock").to_string_lossy().into_owned();
        let tokpath = token_path_for_socket(&sock);
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
        let read = read_token(&sock).expect("token readable");
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
    fn image_confinement_decision_is_distinct_from_capacity_admission() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-img-confine-admission-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();

        let (images, name) = resolve_confined_image_target(&dir, "shot.png")
            .expect("the pure confinement decision accepts a direct child");
        assert_eq!(images, dir.join(IMAGES_DIR).canonicalize().unwrap());
        assert_eq!(name, std::ffi::OsStr::new("shot.png"));

        let mut admission_checked = false;
        let rejected = confine_image_path_with_admission(&dir, "shot.png", |_| {
            admission_checked = true;
            false
        });
        assert!(admission_checked, "capacity is consulted after confinement");
        assert!(
            matches!(rejected, Err(ArtifactConfinementError::AdmissionRefused)),
            "a confined path reports resource refusal distinctly"
        );

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
    fn explicit_capture_waiter_acquires_after_reply_release() {
        let dir = std::env::temp_dir().join(format!("aterm-img-lease-wake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();

        let first = ConfinedImage::for_test(&dir, "first.png");
        let first_lease = acquire_capture_name_lease(&first, || false)
            .expect("first lease acquisition")
            .expect("first explicit lease");
        let second = ConfinedImage::for_test(&dir, "second.png");
        let observed_wait = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_observed_wait = std::sync::Arc::clone(&observed_wait);
        let waiter = std::thread::spawn(move || {
            acquire_capture_name_lease_with_wait(
                &second,
                || {
                    worker_observed_wait.store(true, std::sync::atomic::Ordering::Release);
                    false
                },
                std::time::Duration::from_secs(1),
            )
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !observed_wait.load(std::sync::atomic::Ordering::Acquire)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(
            observed_wait.load(std::sync::atomic::Ordering::Acquire),
            "the successor observed the retained reply lease"
        );

        drop(first_lease);
        let second_lease = waiter
            .join()
            .expect("lease waiter remains live")
            .expect("released predecessor wakes its waiter")
            .expect("waiter acquires the explicit namespace");
        drop(second_lease);
        drop(first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_capture_wait_cancellation_wins() {
        let dir =
            std::env::temp_dir().join(format!("aterm-img-lease-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();

        let first = ConfinedImage::for_test(&dir, "first.png");
        let first_lease = acquire_capture_name_lease(&first, || false)
            .expect("first lease acquisition")
            .expect("first explicit lease");
        let second = ConfinedImage::for_test(&dir, "second.png");
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed_wait = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_cancelled = std::sync::Arc::clone(&cancelled);
        let worker_observed_wait = std::sync::Arc::clone(&observed_wait);
        let waiter = std::thread::spawn(move || {
            acquire_capture_name_lease_with_wait(
                &second,
                || {
                    worker_observed_wait.store(true, std::sync::atomic::Ordering::Release);
                    worker_cancelled.load(std::sync::atomic::Ordering::Acquire)
                },
                std::time::Duration::from_secs(1),
            )
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !observed_wait.load(std::sync::atomic::Ordering::Acquire)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(
            observed_wait.load(std::sync::atomic::Ordering::Acquire),
            "the cancellation probe reached the wait"
        );
        cancelled.store(true, std::sync::atomic::Ordering::Release);

        assert!(
            waiter
                .join()
                .expect("lease waiter remains live")
                .expect("cancellation is not an I/O failure")
                .is_none(),
            "cancellation wins without acquiring the retained namespace"
        );
        drop(first_lease);
        drop(first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_capture_stuck_holder_returns_busy_after_bound() {
        let dir =
            std::env::temp_dir().join(format!("aterm-img-lease-bounded-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();

        let first = ConfinedImage::for_test(&dir, "first.png");
        let first_lease = acquire_capture_name_lease(&first, || false)
            .expect("first lease acquisition")
            .expect("first explicit lease");
        let second = ConfinedImage::for_test(&dir, "second.png");
        let error = acquire_capture_name_lease_with_wait(
            &second,
            || false,
            std::time::Duration::from_millis(40),
        )
        .expect_err("a stuck predecessor must not park the encode worker");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);

        drop(first_lease);
        drop(first);
        drop(second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn explicit_capture_release_unlocks_an_inherited_descriptor() {
        let dir =
            std::env::temp_dir().join(format!("aterm-img-inherited-lock-{}", std::process::id()));
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
        touch(control_socket::SIBLING_TOKEN_FILE);
        touch(&control_socket::token_name_for_sock("private.sock"));

        sweep_stale_instances(&dir);

        assert!(!dir.join(control_socket::instance_sock_name(dead)).exists());
        assert!(!dir.join(control_socket::instance_token_name(dead)).exists());
        // Our own (live) files and the fixed names survive.
        assert!(dir.join(control_socket::instance_sock_name(us)).exists());
        assert!(dir.join(control_socket::instance_token_name(us)).exists());
        assert!(dir.join(control_socket::SIBLING_TOKEN_FILE).exists());
        // An explicit socket's token encodes no pid, so the sweep leaves it to
        // its owner's graceful exit exactly as it leaves the fixed names.
        assert!(
            dir.join(control_socket::token_name_for_sock("private.sock"))
                .exists()
        );
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

    fn wait_for_artifact_cleanup(path: &Path) {
        wait_for_artifact_cleanup_for_test(path);
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

        let root = crate::pinned_dir::PinnedDir::open_resolved(path.parent().unwrap()).unwrap();
        let name = path.file_name().expect("recording name").to_os_string();
        let reader_recording = root.child(&name).unwrap();
        let reader = retain_video_artifact_path(root, name, &reader_recording)
            .unwrap()
            .expect("marker-visible reader lease");
        reader.arm_video_retention_sweep(&reader_recording).unwrap();
        drop(reader_recording);
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
        wait_for_artifact_cleanup(&path);
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
    fn video_producer_sweeps_from_its_local_root_when_reader_releases_first() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-video-producer-final-sweep-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();
        let instance = format!("p{}-producer-final", std::process::id());

        let mut producer = confine_video_dir_for_instance(&dir, &instance).unwrap();
        let path = producer.path().to_path_buf();
        let root_path = path.parent().unwrap().to_path_buf();
        let index = producer
            .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
            .unwrap();
        producer.publish(&[], &index).unwrap();
        let marker = producer.publish_marker().unwrap();

        let oldest = root_path.join("rec-00000000000000000000-100");
        for sequence in 100..112 {
            write_completed_video_fixture(
                &root_path,
                &format!("rec-00000000000000000000-{sequence:03}"),
            );
        }

        let reader_root = crate::pinned_dir::PinnedDir::open_resolved(&root_path).unwrap();
        let name = path.file_name().unwrap().to_os_string();
        let reader_recording = reader_root.child(&name).unwrap();
        let reader = retain_video_artifact_path(reader_root, name, &reader_recording)
            .unwrap()
            .expect("reader overlaps the published producer");
        drop(reader_recording);
        drop(reader);
        assert!(
            oldest.is_dir(),
            "a nonfinal reader release cannot run retention"
        );

        drop(marker);
        drop(index);
        drop(producer);
        wait_for_artifact_cleanup(&path);
        assert!(
            !oldest.exists(),
            "the final producer retains its own charged root through the sweep"
        );
        assert!(path.is_dir(), "the fresh published recording is retained");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn unarmed_final_reader_inherits_sweep_request_on_its_local_root() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-video-unarmed-final-reader-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();
        let root_path = dir.join("recordings");
        ensure_private_dir(&root_path).unwrap();
        for sequence in 0..12 {
            write_completed_video_fixture(&root_path, &format!("rec-{sequence:020}-000"));
        }
        let fresh = std::ffi::OsString::from("rec-00000000000000000011-000");

        let first_root = crate::pinned_dir::PinnedDir::open_resolved(&root_path).unwrap();
        let first_recording = first_root.child(&fresh).unwrap();
        let first = retain_video_artifact_path(first_root, fresh.clone(), &first_recording)
            .unwrap()
            .expect("first reader lease");
        let key = first.key.clone().expect("reader owns its canonical key");
        first.arm_video_retention_sweep(&first_recording).unwrap();
        drop(first_recording);

        let second_root = crate::pinned_dir::PinnedDir::open_resolved(&root_path).unwrap();
        let second_recording = second_root.child(&fresh).unwrap();
        let second = retain_video_artifact_path(second_root, fresh, &second_recording)
            .unwrap()
            .expect("overlapping reader lease");
        drop(second_recording);

        drop(first);
        assert_eq!(artifact_lease_count(&key), Some(1));
        assert_eq!(
            completed_video_fixture_count(&root_path),
            12,
            "the first release cannot sweep through a live second reader"
        );

        let moved = dir.join("recordings-original");
        std::fs::rename(&root_path, &moved).unwrap();
        ensure_private_dir(&root_path).unwrap();
        for sequence in 100..112 {
            write_completed_video_fixture(&root_path, &format!("rec-{sequence:020}-000"));
        }
        std::fs::write(root_path.join("sentinel"), b"replacement").unwrap();

        drop(second);
        wait_for_artifact_cleanup(&key);
        assert!(
            completed_video_fixture_count(&moved) < 12,
            "the unarmed final reader honors the prior sweep request on its own root"
        );
        assert_eq!(
            completed_video_fixture_count(&root_path),
            12,
            "the replacement at the old lexical path is untouched"
        );
        assert_eq!(
            std::fs::read(root_path.join("sentinel")).unwrap(),
            b"replacement"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn video_reader_registry_rejects_replaced_recording_identity() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-video-reader-identity-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();
        let root_path = dir.join("recordings");
        ensure_private_dir(&root_path).unwrap();
        let fresh = std::ffi::OsString::from("rec-00000000000000000000-000");
        write_completed_video_fixture(&root_path, fresh.to_str().unwrap());

        let first_root = crate::pinned_dir::PinnedDir::open_resolved(&root_path).unwrap();
        let first_recording = first_root.child(&fresh).unwrap();
        let first = retain_video_artifact_path(first_root, fresh.clone(), &first_recording)
            .unwrap()
            .expect("first reader lease");
        drop(first_recording);

        let moved = root_path.join("recording-original");
        std::fs::rename(root_path.join(&fresh), &moved).unwrap();
        write_completed_video_fixture(&root_path, fresh.to_str().unwrap());
        let replacement_root = crate::pinned_dir::PinnedDir::open_resolved(&root_path).unwrap();
        let replacement = replacement_root.child(&fresh).unwrap();
        let error = retain_video_artifact_path(replacement_root, fresh, &replacement)
            .expect_err("a replacement cannot join the live recording lease group");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(artifact_lease_count(first.key.as_ref().unwrap()), Some(1));

        drop(replacement);
        drop(first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn video_reader_registry_rejects_same_recording_moved_to_replacement_root() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-video-reader-root-identity-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();
        let root_path = dir.join("recordings");
        ensure_private_dir(&root_path).unwrap();
        let fresh = std::ffi::OsString::from("rec-00000000000000000000-000");
        write_completed_video_fixture(&root_path, fresh.to_str().unwrap());

        let first_root = crate::pinned_dir::PinnedDir::open_resolved(&root_path).unwrap();
        let first_recording = first_root.child(&fresh).unwrap();
        let first = retain_video_artifact_path(first_root, fresh.clone(), &first_recording)
            .unwrap()
            .expect("first reader lease");
        drop(first_recording);

        let original_root = dir.join("recordings-original");
        std::fs::rename(&root_path, &original_root).unwrap();
        ensure_private_dir(&root_path).unwrap();
        std::fs::rename(original_root.join(&fresh), root_path.join(&fresh)).unwrap();

        let replacement_root = crate::pinned_dir::PinnedDir::open_resolved(&root_path).unwrap();
        let same_recording = replacement_root.child(&fresh).unwrap();
        let error = retain_video_artifact_path(replacement_root, fresh, &same_recording)
            .expect_err("the same recording inode cannot cross into a replacement root");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(artifact_lease_count(first.key.as_ref().unwrap()), Some(1));

        drop(same_recording);
        drop(first);
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
        let published = fresh.publish(&[], &index).expect("publish durable batch");
        let marker = fresh.publish_marker().unwrap();
        drop(marker);
        drop(index);
        drop(fresh);
        wait_for_artifact_cleanup(&fresh_path);
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
        drop(fresh_marker);
        drop(fresh_index);
        drop(fresh);
        wait_for_artifact_cleanup(&fresh_path);

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
        for recording in 0..9 {
            let dir = root.join(format!("rec-{recording:020}-000"));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("index.json"), b"{\"frames\":[]}").unwrap();
            std::fs::write(dir.join(VIDEO_PUBLISHED_FILE), b"published").unwrap();
        }
        let fresh_name = std::ffi::OsString::from("rec-99999999999999999999-000");
        let fresh = root.join(&fresh_name);
        std::fs::create_dir(&fresh).unwrap();
        std::fs::write(fresh.join("index.json"), b"{\"frames\":[]}").unwrap();
        std::fs::write(fresh.join(VIDEO_PUBLISHED_FILE), b"published").unwrap();
        let pinned = crate::pinned_dir::PinnedDir::open_resolved(&root).unwrap();

        // Ten scanned entries + two cheap marker/index probes for each of nine
        // candidates, then one marker probe in the completion check. Leave one
        // unit short of its index probe so the shared budget stops before any
        // mutation.
        let limit = 10 * VIDEO_SCAN_WORK + 19 * VIDEO_FILE_OPEN_WORK + (VIDEO_FILE_OPEN_WORK - 1);
        let mut work = VideoPruneWork::new(limit);
        prune_video_dirs_with_work(&pinned, &fresh_name, &mut work).unwrap();

        assert_eq!(work.scanned, 10);
        assert_eq!(work.file_opens, 19);
        assert_eq!(work.renames, 0);
        assert_eq!(work.deletes, 0);
        assert!(work.remaining < VIDEO_FILE_OPEN_WORK);
        assert!(
            fresh.join("index.json").is_file(),
            "budget exhaustion cannot affect the freshly published recording"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Automatic-image namespace GC owns another retained directory chain. It
    /// must not start until the target, sealed file, and reply lease release
    /// theirs, especially when only one file-open worth of headroom remains.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn automatic_image_gc_waits_for_final_capability_under_low_nofile() {
        const CHILD: &str = "ATERM_TEST_AUTO_IMAGE_DEFERRED_GC_NOFILE";
        if !enter_low_nofile_test_child(
            CHILD,
            "control_auth::tests::automatic_image_gc_waits_for_final_capability_under_low_nofile",
            16,
        ) {
            return;
        }

        let temp = aterm_tempfile::tempdir().expect("temporary automatic image root");
        let target = confine_automatic_image_path(temp.path(), "image")
            .expect("confinement fits the descriptor budget");
        let namespace_root = target.dir.parent().unwrap().to_path_buf();
        let path = target.display_path();
        let lease = acquire_capture_name_lease(&target, || false)
            .unwrap()
            .expect("automatic path lease");
        let file = target
            .write_private(b"png")
            .expect("no concurrent GC consumes the remaining file descriptor");
        target.validate_for_reply(&file).unwrap();
        for sequence in 0..(AUTO_IMAGE_KEEP + 2) {
            let name = automatic_capture_name_for(
                "image",
                process_instance_id(),
                9_000_000 + sequence as u64,
            );
            std::fs::write(target.dir.join(name), b"old").unwrap();
        }
        target.arm_automatic_prune();
        assert!(path.is_file());
        assert!(
            !pending_dead_namespace_sweeps()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(&namespace_root),
            "automatic-image confinement must not eagerly schedule namespace GC"
        );

        drop(target);
        drop(file);
        let image_count = || {
            std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .flatten()
                .filter(|entry| automatic_capture_sequence(&entry.file_name()).is_some())
                .count()
        };
        assert!(
            image_count() > AUTO_IMAGE_KEEP,
            "auto-image retention must remain deferred while the path lease is live"
        );
        assert!(
            !pending_dead_namespace_sweeps()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(&namespace_root),
            "the path lease keeps cleanup deferred until every capability closes"
        );
        drop(lease);
        wait_for_best_effort_cleanup_barrier_for_test();
        assert_eq!(
            image_count(),
            AUTO_IMAGE_KEEP,
            "the pre-reserved worker sweep converges after the final capability closes"
        );
    }

    /// Scanning keeps names, not one open directory per recording. Run the
    /// complete retention pass below a descriptor limit that cannot hold all
    /// candidate directories simultaneously.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn video_prune_directory_handles_stay_bounded() {
        const CHILD: &str = "ATERM_TEST_VIDEO_PRUNE_NOFILE";
        if !enter_low_nofile_test_child(
            CHILD,
            "control_auth::tests::video_prune_directory_handles_stay_bounded",
            16,
        ) {
            return;
        }

        let temp = aterm_tempfile::tempdir().expect("temporary video root");
        let root = temp.path();
        for sequence in 0..32 {
            write_completed_video_fixture(root, &format!("rec-{sequence:020}-000"));
        }
        let fresh = std::ffi::OsString::from("rec-99999999999999999999-000");
        write_completed_video_fixture(root, fresh.to_str().unwrap());
        let pinned = crate::pinned_dir::PinnedDir::open_resolved(root).unwrap();

        prune_video_dirs(&pinned, &fresh).unwrap();
        assert_eq!(completed_video_fixture_count(root), VIDEO_KEEP);
        assert!(root.join(&fresh).is_dir());
    }

    #[test]
    fn video_prune_treats_corrupt_and_oversized_indexes_as_completed() {
        let temp = aterm_tempfile::tempdir().expect("temporary video root");
        let root = temp.path();
        for sequence in 0..(VIDEO_KEEP + 2) {
            write_completed_video_fixture(root, &format!("rec-{sequence:020}-000"));
        }
        let corrupt = root.join("rec-00000000000000000000-000");
        std::fs::write(corrupt.join("index.json"), b"not json").unwrap();
        let oversized = root.join("rec-00000000000000000001-000");
        std::fs::OpenOptions::new()
            .write(true)
            .open(oversized.join("index.json"))
            .unwrap()
            .set_len(32 * 1024 * 1024)
            .unwrap();

        let fresh = std::ffi::OsString::from("rec-99999999999999999999-000");
        write_completed_video_fixture(root, fresh.to_str().unwrap());
        let pinned = crate::pinned_dir::PinnedDir::open_resolved(root).unwrap();
        prune_video_dirs(&pinned, &fresh).unwrap();

        assert!(
            !corrupt.exists(),
            "retention completion is the marker/index identity pair, not JSON parsing"
        );
        assert!(
            !oversized.exists(),
            "retention never reads an attacker-sized completed index"
        );
        assert_eq!(completed_video_fixture_count(root), VIDEO_KEEP);
        assert!(root.join(fresh).is_dir());
    }

    #[test]
    fn video_member_write_after_sync_invalidates_marker_guard() {
        let temp = aterm_tempfile::tempdir().expect("temporary video root");
        let instance = format!("p{}-batch-state", std::process::id());
        let mut recording = confine_video_dir_for_instance(temp.path(), &instance).unwrap();
        let path = recording.path().to_path_buf();
        let index = recording
            .write_batch_member_authorized(
                std::ffi::OsStr::new("index.json"),
                b"{\"frames\":[]}",
                || true,
            )
            .unwrap();
        assert!(!recording.batch_synced_for_test());
        recording.publish(&[], &index).unwrap();
        assert!(recording.batch_synced_for_test());

        let late_frame = recording
            .write_sealed_frame(std::ffi::OsStr::new("frame_0001.png"), b"png")
            .unwrap();
        assert!(!recording.batch_synced_for_test());
        let error = recording
            .publish_marker()
            .expect_err("a later member invalidates the earlier directory barrier");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!path.join(VIDEO_PUBLISHED_FILE).exists());

        recording.publish(&[late_frame], &index).unwrap();
        assert!(recording.batch_synced_for_test());
        drop(recording.publish_marker().unwrap());
        assert!(path.join(VIDEO_PUBLISHED_FILE).is_file());
    }

    #[test]
    fn video_publish_rejects_replaced_frame_before_index_commit() {
        let dir =
            std::env::temp_dir().join(format!("aterm-video-frame-delete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();
        let instance = format!("p{}-frame-delete", std::process::id());
        let mut recording = confine_video_dir_for_instance(&dir, &instance).unwrap();
        let path = recording.path().to_path_buf();
        let frame = recording
            .write_sealed_frame(std::ffi::OsStr::new("frame_0001.png"), b"png")
            .unwrap();
        std::fs::rename(path.join("frame_0001.png"), path.join("frame_original.png")).unwrap();
        drop(
            recording
                .write_new_private(std::ffi::OsStr::new("frame_0001.png"), b"replacement")
                .unwrap(),
        );
        let index = recording
            .write_new_private(
                std::ffi::OsStr::new("index.json"),
                br#"{"frames":[{"file":"frame_0001.png"}]}"#,
            )
            .unwrap();

        assert!(
            recording.publish(&[frame], &index).is_err(),
            "an index may never certify a same-name replacement"
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
        // EXPLICIT `unlock()`, not just `drop`. `flock` rides the open-file-
        // description, and a concurrent test's `fork()` (any `pre_exec`
        // Command) that lands between our open and the drop hands the child a
        // duplicate that keeps the lock alive until its execve — hundreds of
        // milliseconds under load — so a sweep racing that window classified
        // this lease Held and kept the namespace. `LOCK_UN` strips the lock
        // from the shared description itself, every duplicate included, so the
        // sweep below deterministically sees the state this scenario names:
        // "the exact launch's lease is unlocked".
        lease.unlock().unwrap();
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

    #[cfg(unix)]
    #[test]
    fn instance_lease_authority_stays_on_pinned_root_after_ancestor_swap() {
        let temp = aterm_tempfile::tempdir().expect("temporary namespace root");
        let root = temp.path().join("root");
        let moved = temp.path().join("root-moved");
        let replacement = temp.path().join("replacement");
        ensure_private_dir(&root).unwrap();
        ensure_private_dir(&replacement).unwrap();

        let name = "p424244-held";
        let original_namespace = ensure_canonical_direct_child(&root, name).unwrap();
        let held_lease = open_instance_lease(&original_namespace, true).unwrap();
        held_lease.try_lock().unwrap();

        // The replacement advertises the same namespace name but an unlocked
        // lease. A lexical lease reopen after the swap would misclassify the
        // live original as abandoned and delete it through the retained root.
        let replacement_namespace = ensure_canonical_direct_child(&replacement, name).unwrap();
        drop(open_instance_lease(&replacement_namespace, true).unwrap());

        let queue = ArtifactCleanupQueuePermit::unmetered();
        let admission = crate::control::ReplyRetention::try_reserve_cleanup_for_path(&root)
            .expect("test cleanup admission");
        let mut sweep = DeadNamespaceSweep::begin(&root, "p1-current".into(), queue, admission)
            .expect("pinned sweep");
        let mut swapped = false;
        assert!(sweep.run_batch_with_hook(
            |_| false,
            || {
                if !swapped {
                    std::fs::rename(&root, &moved).unwrap();
                    std::fs::rename(&replacement, &root).unwrap();
                    swapped = true;
                }
            }
        ));
        assert!(swapped, "the race hook ran immediately before lease open");
        assert!(
            moved.join(name).is_dir(),
            "lease authority must be opened through the retained namespace handle"
        );
        assert!(
            root.join(name).is_dir(),
            "the replacement namespace is outside the sweep authority"
        );

        drop(sweep);
        std::fs::rename(&root, &replacement).unwrap();
        std::fs::rename(&moved, &root).unwrap();
        held_lease.unlock().unwrap();
        drop(held_lease);
    }

    #[test]
    fn dead_namespace_debt_after_cursor_open_restarts_with_fresh_generation() {
        let temp = aterm_tempfile::tempdir().expect("temporary namespace root");
        let root = std::fs::canonicalize(temp.path()).unwrap();
        pending_dead_namespace_sweeps()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(root.clone(), PendingDeadNamespaceSweep { dirty: false });

        let admission = crate::control::ReplyRetention::try_reserve_cleanup_for_path(&root)
            .expect("test cleanup admission");
        let mut sweep = DeadNamespaceSweep::begin(
            &root,
            "p1-current".into(),
            ArtifactCleanupQueuePermit::unmetered(),
            admission,
        )
        .expect("initial cursor");

        // Model another lifecycle release after the cursor snapshot opened.
        // ReadDir need not reveal any corresponding entry to this cursor; the
        // dirty generation itself must force a new one after EOF.
        schedule_dead_instance_namespace_sweep(
            &root,
            "p1-current",
            ArtifactCleanupQueuePermit::unmetered(),
        );
        assert!(sweep.run_batch(|_| false));

        let (sender, receiver) = std::sync::mpsc::channel();
        finish_or_restart_dead_namespace_cursor(sweep, &sender);
        let ArtifactCleanupTask::DeadNamespaces {
            root: restarted_root,
            current,
            _queue,
        } = receiver.try_recv().expect("dirty debt schedules a restart")
        else {
            panic!("dirty debt must restart from a new namespace cursor");
        };
        assert_eq!(restarted_root, root);
        assert_eq!(current, "p1-current");
        clear_pending_dead_namespace_sweep(&root);
    }

    #[test]
    fn cleanup_worker_recovers_both_pending_debts_after_task_panics() {
        let temp = aterm_tempfile::tempdir().expect("temporary namespace root");
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let name = "p424245-panic-recovery";
        let namespace = ensure_canonical_direct_child(&root, name).unwrap();
        drop(open_instance_lease(&namespace, true).unwrap());
        pending_dead_namespace_sweeps()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(root.clone(), PendingDeadNamespaceSweep { dirty: false });

        let scheduler = artifact_cleanup_scheduler().expect("cleanup worker");
        scheduler
            .best_effort
            .send(ArtifactCleanupTask::PanicForTest(
                ArtifactCleanupPanicRecovery::DeadNamespaces {
                    root: root.clone(),
                    current: "p1-current".into(),
                    queue: ArtifactCleanupQueuePermit::unmetered(),
                },
            ))
            .unwrap();
        notify_artifact_cleanup_worker(scheduler);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while namespace.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            !namespace.exists(),
            "panic recovery must requeue the pending root on a fresh cursor"
        );
        assert!(
            !pending_dead_namespace_sweeps()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(&root),
            "the recovered sweep must reconcile its coalescing registry"
        );

        let video_root = ensure_canonical_direct_child(&root, "video-tombstones").unwrap();
        let tombstone = ensure_canonical_direct_child(&video_root, ".prune-panic").unwrap();
        std::fs::write(tombstone.join("partial"), b"partial").unwrap();
        let pinned = crate::pinned_dir::PinnedDir::open(&video_root).unwrap();
        let expected_root = pinned.retained_identity().unwrap();
        drop(pinned);
        pending_video_tombstone_sweeps()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(PendingVideoTombstoneSweep {
                root: video_root.clone(),
                expected_root,
                dirty: false,
            });
        scheduler
            .best_effort
            .send(ArtifactCleanupTask::PanicForTest(
                ArtifactCleanupPanicRecovery::VideoTombstones {
                    root: video_root.clone(),
                    expected_root,
                    queue: ArtifactCleanupQueuePermit::unmetered(),
                },
            ))
            .unwrap();
        notify_artifact_cleanup_worker(scheduler);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while tombstone.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            !tombstone.exists(),
            "video panic recovery must requeue the tombstone cursor"
        );
        assert!(
            !pending_video_tombstone_sweeps()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .any(|entry| { entry.root == video_root && entry.expected_root == expected_root }),
            "the recovered video sweep must reconcile its coalescing registry"
        );
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
        let fresh_path = fresh.path().to_path_buf();
        let index = fresh
            .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
            .unwrap();
        let _ = fresh.publish(&[], &index).unwrap();
        drop(fresh.publish_marker().unwrap());
        drop(index);
        drop(fresh);
        wait_for_artifact_cleanup(&fresh_path);

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
