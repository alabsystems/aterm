// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Default-on access control for the control socket, which grants full power over the live
//! terminal (drive the shell, deliver signals, snapshot pixels). Three layers, all
//! transparent to a same-user client:
//!
//! 1. **Per-user private directory.** The socket lives in a directory only the owning user
//!    can traverse ([`socket_dir`]), and the socket file is `0600` after bind.
//! 2. **Peer credential check.** After `accept(2)` a peer whose uid is not our `geteuid()`
//!    is refused, in case the directory perms are ever loosened.
//! 3. **Capability token.** Each launch writes 32 random bytes as hex to its `0600` token
//!    file. A connection's first line must be `AUTH <hex>` (or a `TOKEN <hex> <verb…>`
//!    prefix), compared in constant time, so a same-uid process that cannot read the token
//!    file (a sandboxed peer, a confused deputy) is refused.
//!
//! 1. **Per-user private directory.** The socket lives in a `0700` directory
//!    only the owning user can traverse (`$XDG_RUNTIME_DIR`, else
//!    `~/Library/Application Support/aterm` on macOS and
//!    `~/.local/state/aterm/run` on other Unix), and the socket file itself is
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
//! The filesystem and peer primitives are per platform: `control_auth_unix.rs` and
//! `control_auth_win.rs`, whose module doc states the Windows posture.

use std::path::{Path, PathBuf};

use aterm_types::control_socket::{self, SocketDirective};
use aterm_uds::CtlStream;

#[cfg(unix)]
#[path = "control_auth_unix.rs"]
mod imp;
#[cfg(windows)]
#[path = "control_auth_win.rs"]
mod imp;

// Also read by `native_document_journal` (the key file must be ours) and the tests.
#[cfg(unix)]
pub(crate) use imp::our_uid;
pub(crate) use imp::{ensure_private_dir, lock_socket_file, peer_check, provision_token};
// The directory a control socket and its token go in: on Windows an elevated
// instance holds it to the elevation law (`control_auth_win.rs`,
// `docs/WINDOWS_PARAGON_DESIGN.md` §5.7); everywhere else it is a private dir.
#[cfg(windows)]
pub(crate) use imp::ensure_control_dir;
#[cfg(not(windows))]
pub(crate) use imp::ensure_private_dir as ensure_control_dir;
// Whether a pid is alive: the stale sweeps' one test, shared with the handoff's
// sweep of a dead sender's `seamless-<pid>-…` leftovers (`crate::seamless`).
pub(crate) use imp::pid_alive;
// `peer_uid`: `peer_check` inside the unix `imp`, the macOS handoff rendezvous dialer
// (`handoff_rendezvous::dial_and_claim` proves the listener is same-uid before it writes the
// claim secret), and the socketpair unit test.
#[cfg(all(unix, any(target_os = "macos", test)))]
pub(crate) use imp::peer_uid;

/// Atomically (re)point the `latest` alias at this instance's socket
/// ([`aterm_uds::latest::publish`]): a symlink on Unix, a pointer file holding the relative
/// sock name on Windows, renamed over the alias so a client never sees it missing.
/// Best-effort: clients can still target the instance socket (`aterm-ctl --pid`).
pub(crate) fn publish_latest_link(link: &Path, sock_path: &str) {
    aterm_uds::latest::publish(link, sock_path);
}

/// 32 random bytes as 64 lowercase hex chars, from the workspace's one audited entropy
/// surface ([`aterm_uds::rand`]), never a hand-rolled device read. `None` when no entropy
/// source is available; the caller must then refuse to start the socket (fail closed).
#[must_use]
pub(crate) fn random_token_hex() -> Option<String> {
    aterm_uds::rand::hex_token::<32>().ok()
}

/// Filename of the `latest` alias in the per-user directory, naming the newest instance's
/// `aterm-<pid>.sock`.
pub(crate) const SOCK_FILE: &str = control_socket::LATEST_SOCK_FILE;

/// Subdirectory of the socket directory that confines `image`-verb PNG writes.
pub(crate) const IMAGES_DIR: &str = "images";

/// Server-auto-named captures live below this private child, separated from
/// caller-explicit filenames so retention can never delete an explicit target.
const AUTO_IMAGES_DIR: &str = "auto";

/// Video introspection recordings subdir (frame sequences + index.json).
pub(crate) const VIDEO_DIR: &str = "video";
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

/// Run the calling test alone in a child of the test binary
/// ([`crate::test_alone::alone`]) whose soft `RLIMIT_NOFILE` leaves exactly
/// `spare_descriptors` free: `true` in that child, which runs the body, and
/// `false` in the lib-test process, whose limit — process-wide — is untouched.
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[track_caller]
pub(crate) fn enter_low_nofile_test_child(spare_descriptors: usize) -> bool {
    if !crate::test_alone::alone() {
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

/// How long an explicit capture waits for the previous reply's retention guard before it
/// refuses: long enough that overlapping drivers queue (a guard is released at its client's
/// acknowledgement), short enough that a stuck holder is reported rather than hanging the
/// control thread.
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
        let nonce = random_token_hex()
            .map(|token| token[..24].to_string())
            .unwrap_or_else(|| {
                aterm_time::since_epoch()
                    .map_or(0, |duration| duration.as_nanos())
                    .to_string()
            });
        format!("p{}-{nonce}", std::process::id())
    })
}

fn automatic_capture_name_for(stem: &str, instance: &str, sequence: u64) -> String {
    format!("{stem}-{instance}-{sequence:06}.png")
}

/// Mint a server-unique omitted-name capture filename. Explicit client names keep their
/// overwrite semantics.
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
    os_lock: Option<crate::native_document_host::HeldAdvisoryLock>,
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
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ArtifactReaderLease",
        action = "Acquire",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reader_lease"
    )
)]
fn artifact_reader_acquire_anchor() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ArtifactReaderLease",
        action = "RejectAcquireWhileSweeping",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reader_lease"
    )
)]
fn artifact_reader_reject_acquire_anchor() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ArtifactReaderLease",
        action = "RejectReplacedIdentity",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reader_lease"
    )
)]
fn artifact_reader_reject_replaced_identity_anchor() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ArtifactReaderLease",
        action = "Arm",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reader_lease"
    )
)]
fn artifact_reader_arm_anchor() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ArtifactReaderLease",
        action = "Release",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reader_lease"
    )
)]
fn artifact_reader_release_anchor() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ArtifactReaderLease",
        action = "StartSweep",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reader_lease"
    )
)]
fn artifact_reader_start_sweep_anchor() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
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

/// One reserved place in the cleanup queue, shared by every clone of the task
/// that carries it (a panic's recovery copy) and given back when the last goes.
#[derive(Clone, Debug)]
struct ArtifactCleanupQueuePermit {
    _slot: std::sync::Arc<crate::budget::Slot>,
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
        budgets: &crate::control::ArtifactBudgets,
    ) -> Option<std::sync::Arc<Self>> {
        Some(std::sync::Arc::new(Self {
            image_dir,
            image_identity,
            fresh,
            image_queue: Some(reserve_artifact_cleanup_queue(budgets)?),
            image_armed: std::sync::atomic::AtomicBool::new(false),
            namespace_root,
            current,
            namespace_queue: Some(reserve_artifact_cleanup_queue(budgets)?),
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
    fn try_reserve(budgets: &crate::control::ArtifactBudgets) -> Option<Self> {
        Some(Self {
            _slot: std::sync::Arc::new(budgets.try_reserve_cleanup_queue()?),
        })
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
    /// Parks the worker between two rendezvous on the barrier — see
    /// [`hold_artifact_cleanup_for_test`].
    #[cfg(all(test, unix))]
    Hold(std::sync::Arc<std::sync::Barrier>),
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
    #[cfg(all(test, unix))]
    Hold,
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
            #[cfg(all(test, unix))]
            Self::Hold(_) => ArtifactCleanupPanicRecovery::Hold,
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
                crate::control::ArtifactBudgets::process(),
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

fn reserve_artifact_cleanup_queue(
    budgets: &crate::control::ArtifactBudgets,
) -> Option<ArtifactCleanupQueuePermit> {
    artifact_cleanup_scheduler()?;
    ArtifactCleanupQueuePermit::try_reserve(budgets)
}

fn reserve_video_retention(
    path: &Path,
    budgets: &crate::control::ArtifactBudgets,
) -> Option<crate::control::VideoRetentionPermit> {
    artifact_cleanup_scheduler()?;
    budgets.try_reserve_video_retention_for_path(path)
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
        // A hold owns no work to resume.
        #[cfg(all(test, unix))]
        ArtifactCleanupPanicRecovery::Hold => {}
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

/// One lease-registry entry as the artifact Tier-1 binds read it.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ArtifactLeaseEntryForTest {
    pub(crate) count: usize,
    pub(crate) sweep_requested: bool,
    pub(crate) sweeping: bool,
    /// The entry still holds the retention admission it reserved on entry.
    pub(crate) admission_held: bool,
}

/// The registry entry for `key`, or `None` once no lease holds it.
#[cfg(test)]
pub(crate) fn artifact_lease_registry_for_test(key: &Path) -> Option<ArtifactLeaseEntryForTest> {
    let (leases, _) = artifact_path_leases();
    let held = leases
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    held.get(key).map(|state| ArtifactLeaseEntryForTest {
        count: state.count,
        sweep_requested: state.video_sweep_requested,
        sweeping: state.sweeping,
        admission_held: state.video_retention.is_some(),
    })
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
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
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

/// Park the process-wide cleanup worker until the returned guard drops, and
/// return only once it is parked. A retention task queued meanwhile can neither
/// run nor finish, so a sweep the caller starts stays open for as long as it
/// needs to judge it. Every test in the process shares the worker: hold it only
/// across non-blocking registry calls, never across a wait for cleanup.
#[cfg(all(test, unix))]
pub(crate) fn hold_artifact_cleanup_for_test() -> ArtifactCleanupHold {
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    assert!(try_schedule_artifact_cleanup(ArtifactCleanupTask::Hold(
        std::sync::Arc::clone(&gate)
    )));
    gate.wait();
    ArtifactCleanupHold(gate)
}

/// Releases the worker parked by [`hold_artifact_cleanup_for_test`].
#[cfg(all(test, unix))]
pub(crate) struct ArtifactCleanupHold(std::sync::Arc<std::sync::Barrier>);

#[cfg(all(test, unix))]
impl Drop for ArtifactCleanupHold {
    fn drop(&mut self) {
        self.0.wait();
    }
}

#[cfg(all(test, unix))]
fn wait_for_best_effort_cleanup_barrier_for_test() {
    let (done, completed) = std::sync::mpsc::sync_channel(1);
    assert!(try_schedule_artifact_cleanup(ArtifactCleanupTask::Barrier(
        done
    )));
    completed
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
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
    budgets: &crate::control::ArtifactBudgets,
) {
    match task {
        ArtifactCleanupTask::AutomaticImages {
            dir,
            expected_dir,
            fresh,
            _queue,
        } => {
            let Some(admission) = budgets.try_reserve_cleanup_for_path(&dir) else {
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
            #[cfg(test)]
            race_window(RaceWindow::ImagesPinned);
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
            let Some(admission) = budgets.try_reserve_cleanup_for_path(&root) else {
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
            let Some(admission) = budgets.try_reserve_cleanup_for_path(&root) else {
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
        #[cfg(all(test, unix))]
        ArtifactCleanupTask::Hold(gate) => {
            gate.wait();
            gate.wait();
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
    budgets: &crate::control::ArtifactBudgets,
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
                run_best_effort_cleanup_task(task, best_effort_sender, budgets);
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

pub(crate) fn acquire_capture_name_lease_with_wait(
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
        // Lock the same retained authority the writer uses. On Unix the
        // directory inode itself is the advisory-lock object, so a same-uid
        // actor cannot split two writers by replacing a child lockfile. Windows
        // locks a child file proven to sit inside the pinned directory; its
        // share mode denies its own replacement, and NTFS refuses to rename a
        // directory above an open file, for the full lock lifetime.
        #[cfg(unix)]
        let mut file = target.pinned()?.open_directory_lock()?;
        #[cfg(windows)]
        let mut file = {
            let lock_dir = target
                .pinned()?
                .ensure_child(std::ffi::OsStr::new(CAPTURE_LOCK_DIR))?;
            lock_dir.open_namespace_lock(std::ffi::OsStr::new(CAPTURE_NAMESPACE_LEASE_FILE))?
        };
        // Another process's capture queues for the same bounded wait. One held past it is
        // named: a reply its client has not acknowledged (`ACK <nonce>`) is held until the
        // ACK, or for the handoff and quarantine intervals after it.
        loop {
            match crate::native_document_host::HeldAdvisoryLock::try_lock_keeping(file) {
                Ok(held) => {
                    lease.os_lock = Some(held);
                    break;
                }
                Err((refused, std::fs::TryLockError::WouldBlock)) => {
                    file = refused;
                    if cancelled() {
                        return Ok(None);
                    }
                    if waited_from.elapsed() >= explicit_wait {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::WouldBlock,
                            "another aterm process owns the shared image namespace — a capture \
                             reply there is not acknowledged yet (it is released at its \
                             client's ACK, else up to a minute later); `image --bytes` needs \
                             no namespace",
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err((_, std::fs::TryLockError::Error(error))) => return Err(error),
            }
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
    budgets: &crate::control::ArtifactBudgets,
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
        let video_retention = reserve_video_retention(root.path(), budgets).ok_or_else(|| {
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
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
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
        // Release the cross-process fence before waking a local waiter. The
        // guard's drop unlocks, which matters on Unix: `flock` follows the
        // open-file description into duplicated/fork-inherited descriptors, so
        // merely dropping this process's handle can leave a transient lock behind
        // in a child between fork and exec. Crashes still release the lock when
        // every inherited handle closes, without a pathname deletion/recreation
        // race.
        drop(self.os_lock.take());
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

fn valid_instance_component(instance: &str) -> bool {
    !instance.is_empty()
        && instance
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && crate::pinned_dir::is_one_component(std::ffi::OsStr::new(instance))
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
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Test-only: a point between one of this module's checks and the re-check
/// that repeats it before the use. The re-checks exist for a swap that lands
/// in that window and nowhere else; [`race_window`] is where a test lands one.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RaceWindow {
    /// [`ensure_canonical_direct_child`]: the child has been looked at.
    ChildLooked,
    /// [`open_private_namespace_lock`]: the lock has been looked at, not yet opened.
    LockLooked,
    /// [`open_private_namespace_lock`]: the lock is open, its path not yet re-checked.
    LockOpened,
    /// A sweep cursor's `begin`: its root is pinned and not yet listed.
    CursorPinned,
    /// The automatic-image sweep: its directory is pinned, its path not yet re-checked.
    ImagesPinned,
    /// `publish_marker`: the marker is visible and retention not yet armed.
    MarkerVisible,
}

/// A window armed with the swap to run there once.
#[cfg(test)]
type ArmedRace = (RaceWindow, Box<dyn FnOnce()>);

#[cfg(test)]
std::thread_local! {
    /// The race armed on this thread.
    static ARMED_RACE: std::cell::RefCell<Option<ArmedRace>> =
        const { std::cell::RefCell::new(None) };
}

/// Run the swap armed for `at` on this thread, if one is, and disarm it.
#[cfg(test)]
fn race_window(at: RaceWindow) {
    let swap = ARMED_RACE.with_borrow_mut(|armed| {
        if armed.as_ref().is_some_and(|(window, _)| *window == at) {
            armed.take().map(|(_, swap)| swap)
        } else {
            None
        }
    });
    if let Some(swap) = swap {
        swap();
    }
}

/// Create/validate one real directory immediately below a canonical parent.
/// Existing links/reparse redirections are rejected before hardening or use.
///
/// `name` is judged raw ([`crate::pinned_dir::is_one_component`]): `Path::components()`
/// counts `"x/"` as one component, and `parent.join("x/")` resolves a link `x`, even a
/// dangling one, at the checks and the create below.
fn ensure_canonical_direct_child(parent: &Path, name: &str) -> std::io::Result<PathBuf> {
    if !crate::pinned_dir::is_one_component(std::ffi::OsStr::new(name)) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "child name is not one path component",
        ));
    }
    let parent = std::fs::canonicalize(parent)?;
    let child = parent.join(name);
    let looked = std::fs::symlink_metadata(&child);
    #[cfg(test)]
    race_window(RaceWindow::ChildLooked);
    match looked {
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
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
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
    // Judged raw, as `ensure_canonical_direct_child` judges its name.
    if !crate::pinned_dir::is_one_component(file_name) {
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
    #[cfg(test)]
    race_window(RaceWindow::LockLooked);
    let file = options.open(&path)?;
    #[cfg(test)]
    race_window(RaceWindow::LockOpened);
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
    type Held = crate::native_document_host::HeldAdvisoryLock;
    static LEASES: std::sync::OnceLock<std::sync::Mutex<Vec<(PathBuf, Held)>>> =
        std::sync::OnceLock::new();
    let namespace = std::fs::canonicalize(namespace)?;
    let leases = LEASES.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    let mut leases = leases
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if leases.iter().any(|(path, _)| path == &namespace) {
        return Ok(());
    }
    let held =
        Held::try_lock(open_instance_lease(&namespace, true)?).map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "this process instance namespace is already leased",
            ),
            std::fs::TryLockError::Error(error) => error,
        })?;
    leases.push((namespace, held));
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
        #[cfg(test)]
        race_window(RaceWindow::CursorPinned);
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
        self.run_batch_with_hook(DEAD_INSTANCE_DELETE_LIMIT, pid_alive, || {})
    }

    /// [`Self::run_batch`] with its delete budget given and a hook run just
    /// before each namespace's lease probe.
    fn run_batch_with_hook(
        &mut self,
        delete_limit: usize,
        mut pid_alive: impl FnMut(u32) -> bool,
        mut before_lease_probe: impl FnMut(),
    ) -> bool {
        if self.root.validate_path_identity().is_err() {
            return true;
        }
        let mut delete_budget = delete_limit;
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
            // An acquired lease is held by a guard that unlocks (`LOCK_UN`) before the close
            // on every way out of this iteration. `flock` lives on the open file description:
            // a child forked or posix_spawned meanwhile (Darwin hands it the whole table)
            // holds a copy until it execs, so a lease released by the close alone stays
            // locked there and the next sweep reads the dead instance as live.
            let (lease_state, acquired_lease) = match namespace
                .open_existing_namespace_lock_at_retained(std::ffi::OsStr::new(INSTANCE_LEASE_FILE))
            {
                Ok(lease) => match crate::native_document_host::HeldAdvisoryLock::try_lock(lease) {
                    Ok(held) => {
                        #[cfg(all(test, unix))]
                        crate::parked_fork::fork_if_armed();
                        (InstanceLeaseState::Acquirable, Some(held))
                    }
                    Err(std::fs::TryLockError::WouldBlock) => (InstanceLeaseState::Held, None),
                    Err(std::fs::TryLockError::Error(_)) => (InstanceLeaseState::Invalid, None),
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    (InstanceLeaseState::Missing, None)
                }
                Err(_) => (InstanceLeaseState::Invalid, None),
            };
            // PID is the compatibility authority only for a pre-lease namespace.
            let legacy_pid_alive = lease_state == InstanceLeaseState::Missing && pid_alive(pid);
            if decide_instance_namespace_sweep(lease_state, legacy_pid_alive)
                == InstanceSweepDecision::Remove
            {
                // Keep a successful exact-lease acquisition alive across the exact
                // rename and bounded cleanup. Legacy/no-lease removal carries no
                // guard by definition.
                #[cfg(not(windows))]
                let _acquired_lease = acquired_lease;
                // Windows cannot keep it: NTFS refuses to rename a directory while
                // a file inside it is open, so the held lease answered
                // ERROR_ACCESS_DENIED to the rename below and no dead launch's
                // namespace was ever swept (6ad1a274d8's measurement, 2026-09-28).
                // Letting go first is safe: the acquisition proved the launch gone,
                // the instance name carries its launch nonce so no later launch can
                // take this lease, and a sweeper that locks it in the gap only makes
                // this rename fail, which keeps the namespace for a later pass.
                #[cfg(windows)]
                drop(acquired_lease);
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
    let budgets = crate::control::ArtifactBudgets::shipping();
    let queue = ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("a fresh queue has room");
    let Some(admission) = budgets.try_reserve_cleanup_for_path(root) else {
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
pub(crate) struct ConfinedVideoDir {
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
        #[cfg(test)]
        race_window(RaceWindow::CursorPinned);
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
        self.run_batch_within(VIDEO_PRUNE_WORK_LIMIT)
    }

    /// [`Self::run_batch`] with its work budget given.
    fn run_batch_within(&mut self, work_limit: usize) -> bool {
        if self.root.validate_path_identity().is_err() {
            return true;
        }
        let mut work = VideoPruneWork::new(work_limit);
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
/// Retaining each frame's [`crate::pinned_dir::PinnedFile`] until the reply is acknowledged
/// costs a descriptor per frame and can exhaust macOS's default soft limit, so this records
/// the filename and its device/inode (Unix) or volume/file-index (Windows), and validation
/// reopens one frame at a time. It detects replacement at the wire edge, not continuous
/// exclusion: a same-user process can mutate a closed file, and identities can be reused.
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
    #[cfg(all(test, unix))]
    pub(crate) fn is_published(&self) -> bool {
        self.published
    }

    #[must_use]
    pub(crate) fn path(&self) -> &Path {
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

    /// Write one frame (its bytes ordered ahead of the marker, `aterm_durable::ordered`),
    /// then close its per-file descriptor and retain
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

    /// Write a frame/index batch member, its bytes ordered ahead of the marker, without
    /// syncing the recording
    /// directory yet. Per-member work stays on the retained recording
    /// capability and validates only the renamed entry; [`Self::publish`] is the
    /// mandatory full-path batch barrier before the visibility marker exists.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
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

    pub(crate) fn abort(mut self) -> std::io::Result<()> {
        self.abort_in_place()
    }

    /// Publish only after `index.json` is flushed. Marker publication requests
    /// one retention sweep, which the final producer-or-reader lease performs;
    /// retention never runs at mint or in this fallible pre-marker phase, so a
    /// refused/failed request never deletes a prior good recording. This is also
    /// the required parent-directory durability barrier for all deferred
    /// frame/index writes.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
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
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "VideoBatchPublicationDurability",
            action = "PublishMarker",
            project = "aterm_gui::artifact_transaction_conformance::project_video_batch_publication_durability"
        )
    )]
    pub(crate) fn publish_marker(&mut self) -> std::io::Result<crate::pinned_dir::PinnedFile> {
        self.publish_marker_with_hook(|| {})
    }

    #[cfg(all(test, unix))]
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
                #[cfg(test)]
                race_window(RaceWindow::MarkerVisible);
                if let Err(error) = retention_lease.arm_video_retention_sweep(recording) {
                    arm_error = Some(error);
                }
                after_publish();
            },
            // Ordered: nothing depends on the marker after a power loss; a recording
            // that never became visible is cleaned as a tombstone, and the requester is
            // local.
            crate::durable::Flush::Ordered,
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

/// Create and pin a fresh server-named recording directory under `video/<process-instance>/`.
/// No path component is client-controlled: the launch namespace and recording name are
/// minted here, and every file inside is written through the retained directory capability.
#[must_use]
#[cfg(test)]
pub(crate) fn confine_video_dir(sock_dir: &Path) -> Option<ConfinedVideoDir> {
    confine_video_dir_with_admission(
        sock_dir,
        &crate::control::ArtifactBudgets::shipping(),
        |_| true,
    )
    .ok()
}

pub(crate) fn confine_video_dir_with_admission(
    sock_dir: &Path,
    budgets: &crate::control::ArtifactBudgets,
    admit: impl FnOnce(&Path) -> bool,
) -> Result<ConfinedVideoDir, ArtifactConfinementError> {
    confine_video_dir_for_instance_with_admission(sock_dir, process_instance_id(), budgets, admit)
}

/// The tests' confinement, against a budget set of its own at the shipping
/// caps ([`crate::control::ArtifactBudgets::shipping`]): no sibling test's
/// recordings hold its cleanup-queue places, so its admission is never refused
/// for capacity.
#[must_use]
#[cfg(test)]
fn confine_video_dir_for_instance(sock_dir: &Path, instance: &str) -> Option<ConfinedVideoDir> {
    confine_video_dir_for_instance_with_admission(
        sock_dir,
        instance,
        &crate::control::ArtifactBudgets::shipping(),
        |_| true,
    )
    .ok()
}

fn confine_video_dir_for_instance_with_admission(
    sock_dir: &Path,
    instance: &str,
    budgets: &crate::control::ArtifactBudgets,
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
        reserve_video_retention(instance_dir.path(), budgets)
            .ok_or(ArtifactConfinementError::AdmissionRefused)?,
    );
    let tombstone_queue = reserve_artifact_cleanup_queue(budgets)
        .ok_or(ArtifactConfinementError::AdmissionRefused)?;
    let dead_queue = reserve_artifact_cleanup_queue(budgets)
        .ok_or(ArtifactConfinementError::AdmissionRefused)?;
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
    confine_automatic_image_path_with_admission(
        sock_dir,
        stem,
        &crate::control::ArtifactBudgets::shipping(),
        |_| true,
    )
    .ok()
}

pub(crate) fn confine_automatic_image_path_with_admission(
    sock_dir: &Path,
    stem: &str,
    budgets: &crate::control::ArtifactBudgets,
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
        budgets,
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

/// Run the automatic-image retention sweep synchronously (the `ArtifactReplyPublication`
/// Tier-1 bind). The sweep is a best-effort bound on completed auto-named files in this
/// process's namespace: each completion runs one bounded scan, so server-named overflow
/// converges across sweeps; same-uid undeletable clutter may keep it above
/// [`AUTO_IMAGE_KEEP`], and it never delays or invalidates the fresh reply.
#[cfg(test)]
pub(crate) fn prune_automatic_images_for_test(target: &ConfinedImage) {
    prune_automatic_image_dir(target);
}

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

/// Recordings kept on disk, including the one just created. Each can hold a full frame
/// budget of PNGs (hundreds of MiB at `full`), so a looping recorder would otherwise grow
/// its namespace without bound. Marker publication requests a sweep; the final
/// producer-or-reader lease performs it, oldest first.
const VIDEO_KEEP: usize = 8;

/// Video retention (`prune_video_dirs`) deletes completed recordings beyond [`VIDEO_KEEP`],
/// never `fresh`, oldest first: server-named `rec-<sequence>-<collision>` names sort in
/// creation order within a namespace. Best-effort: an undeletable entry is skipped, and
/// retention never fails a recording. A recording is completed only with both `index.json`
/// and the `.published` marker; an in-flight one has no marker and is not `fresh` for a later
/// `video` call, so without that gate a second recording's prune could destroy it before its
/// result is delivered. These limits bound one sweep's work.
const VIDEO_PRUNE_WORK_LIMIT: usize = 64 * 1024 * 1024;
const VIDEO_PRUNE_SCAN_LIMIT: usize = 512;
const VIDEO_PRUNE_DELETE_ENTRY_LIMIT: usize = 16_384;
const VIDEO_SCAN_WORK: usize = 64;
const VIDEO_FILE_OPEN_WORK: usize = 4 * 1024;
const VIDEO_DELETE_WORK: usize = 4 * 1024;

/// One shared retention allowance covers directory scans, file probes, renames and deletes
/// across a whole sweep, so a crowded namespace cannot multiply retention work.
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
        // The pin the rename moved clears and removes the tombstone itself, on
        // both backends: Unix by its descriptor, Windows by where its handle now
        // is (`pinned_dir`'s `remove_contents`), never by the stale old name.
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

/// Resolve the per-user directory holding the control socket, token and image subdir, and
/// make it private ([`ensure_private_dir`]: `0700` on Unix, an owner-only DACL on Windows).
///
/// Order of preference (matched exactly by the `aterm-ctl` client):
/// 1. `$XDG_RUNTIME_DIR/aterm` when `XDG_RUNTIME_DIR` is set (already a
///    per-user `0700` dir on systems that provide it).
/// 2. `~/Library/Application Support/aterm` on macOS (the conventional
///    per-user app-support location), and on every other Unix
///    `${XDG_STATE_HOME:-~/.local/state}/aterm/run` (under aterm's XDG state
///    folder), created `0700`.
/// 3. Windows: `%TEMP%\aterm` (`%TMP%`/`%TEMP%`, else `%LOCALAPPDATA%\Temp\aterm`)
///    — deliberately OUTSIDE the OneDrive-managed `%APPDATA%` subtree, where afunix
///    `connect` cannot reach the socket's reparse point (WSAEINVAL). See
///    [`aterm_uds::control_socket_dir`]. Short enough for the 108-byte `sun_path`.
///
/// The location is [`aterm_uds::control_socket_dir`], shared with the client so the two
/// cannot drift: `$XDG_RUNTIME_DIR/aterm`, else `~/Library/Application Support/aterm`; on
/// Windows `%TEMP%\aterm`, outside the OneDrive-managed `%APPDATA%` where afunix `connect`
/// fails `WSAEINVAL`.
///
/// `None` when the per-user base cannot be resolved or cannot be made private;
/// [`resolve_socket_plan`] tells the two apart.
#[must_use]
pub(crate) fn socket_dir() -> Option<PathBuf> {
    let dir = aterm_uds::control_socket_dir()?;
    ensure_private_dir(&dir).ok()?;
    Some(dir)
}

/// `<control dir>/<name>`, created `0700`: where the shell integration's
/// per-session files live (`integration`, `rekey`). `None` when there is no
/// private control dir.
pub(crate) fn private_subdir(name: &str) -> Option<PathBuf> {
    let dir = socket_dir()?.join(name);
    ensure_private_dir(&dir).ok()?;
    Some(dir)
}

/// Session `sid`'s file in `dir`. Only a server-minted sid
/// ([`crate::keeper_link::valid_sid`]) is accepted, so no carried value can steer
/// a write out of the directory.
pub(crate) fn sid_file_in(dir: &Path, sid: &aterm_session::SessionId) -> Option<PathBuf> {
    crate::keeper_link::valid_sid(sid.as_str()).then(|| dir.join(sid.as_str()))
}

/// Everything the server needs to provision one instance's control socket.
#[derive(Clone)]
pub(crate) struct SocketPlan {
    /// Path to bind the listening socket at.
    pub sock_path: String,
    /// Path of this instance's capability-token file.
    pub token_path: PathBuf,
    /// The `latest` alias to maintain (`None` for an explicit `--control-sock` override,
    /// which owns its path outright).
    pub latest_link: Option<PathBuf>,
}

/// How the control socket should be provisioned this launch.
pub(crate) enum SocketResolution {
    /// Bind per this plan.
    Enabled(SocketPlan),
    /// Disabled by `--no-control-sock` or `--control-sock 0`/`off`; do not bind.
    Disabled,
    /// No per-user directory and no override resolvable; do not bind.
    NoDir,
    /// The per-user directory resolved, but it cannot be made private to this
    /// user ([`ensure_private_dir`]: not creatable, not ours, or shared); do
    /// not bind. `error` is that refusal's text, for the log.
    DirUnusable { dir: PathBuf, error: String },
    /// `--control-sock` names a path too long for `sun_path`; do not bind. `bind` and
    /// `connect` both fail `EINVAL` there, which the fail-safe liveness probe reads as
    /// maybe-live, so the warning names the limit instead.
    PathTooLong { path: String, limit: usize },
}

/// The longest socket path `sockaddr_un` can carry on this platform, excluding
/// the NUL terminator: `sun_path` is 104 bytes on the BSDs/macOS and 108 on
/// Linux/Windows (`UNIX_PATH_MAX`), one of which the kernel spends on the NUL.
pub(crate) const MAX_SUN_PATH: usize = if cfg!(any(
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
pub(crate) fn sun_path_ok(path: &str) -> bool {
    path.len() <= MAX_SUN_PATH
}

/// The socket plan. `--control-sock <path>` names an explicit path or, with `0`/`off` (as
/// `--no-control-sock`), disables the socket; no flag means the per-instance
/// `aterm-<pid>.sock` in [`socket_dir`], published via the `aterm.sock` alias. The decision
/// is engine-side ([`control_socket::socket_directive`]).
#[must_use]
pub(crate) fn resolve_socket_plan() -> SocketResolution {
    resolve_socket_plan_for(
        crate::cli::launch_flags().socket_directive(),
        aterm_uds::control_socket_dir,
    )
}

/// [`resolve_socket_plan`] for a given directive and per-user directory lookup.
fn resolve_socket_plan_for(
    directive: SocketDirective,
    control_socket_dir: impl FnOnce() -> Option<PathBuf>,
) -> SocketResolution {
    match directive {
        SocketDirective::Disabled => SocketResolution::Disabled,
        SocketDirective::Explicit(p) => {
            // An oversized path can never bind (EINVAL), and its liveness probe would misread
            // that as maybe-live.
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
        // `socket_dir`'s two refusals, told apart: no folder to resolve, and a
        // folder that cannot be made private.
        SocketDirective::PerInstance => {
            let Some(dir) = control_socket_dir() else {
                return SocketResolution::NoDir;
            };
            if let Err(error) = ensure_private_dir(&dir) {
                return SocketResolution::DirUnusable {
                    dir,
                    error: error.to_string(),
                };
            }
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
    }
}

/// Remove per-instance sockets and tokens of instances whose pid is dead ([`pid_alive`]): a
/// crashed session or an update handoff's predecessor leaves them. Live instances, the fixed
/// filenames, and the pair the `latest` alias ([`SOCK_FILE`]) names are kept; the next bind
/// repoints the alias and the sweep after that takes its old target.
pub(crate) fn sweep_stale_instances(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let names: Vec<String> = entries
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let aliased = aterm_uds::latest::target_name(&dir.join(SOCK_FILE))
        .and_then(|target| control_socket::instance_pid(&target.to_string_lossy()));
    for stale in control_socket::stale_instance_files(&refs, &imp::pid_alive) {
        if aliased.is_some() && control_socket::instance_pid(&stale) == aliased {
            continue;
        }
        let _ = std::fs::remove_file(dir.join(stale));
    }
}

/// [`control_socket::STALE_SOCK_MIN_AGE_SECS`] — the day a zero-length socket
/// file must have sat untouched before the launch sweep probes it — as the
/// `Duration` the filesystem half works in.
#[cfg(any(windows, test))]
const STALE_SOCK_MIN_AGE: std::time::Duration =
    std::time::Duration::from_secs(control_socket::STALE_SOCK_MIN_AGE_SECS);

/// What one directory's launch sweep did, for its one log line.
#[cfg(any(windows, test))]
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SockSweep {
    /// Stale files removed, in listing order.
    pub removed: Vec<String>,
    /// Stale files the OS would not let go of (`remove_file` failed), named so
    /// the log says what is still there; the next launch simply retries.
    pub failed: Vec<String>,
}

/// Remove the zero-length `*.sock` files in `dir` at least `min_age` old that nothing listens
/// on ([`control_socket::stale_socket_files`] decides). `None` when `dir` cannot be listed.
///
/// Facts come from the directory listing, not a per-entry `stat`: on Windows opening another
/// process's live socket file fails (`ERROR_CANT_ACCESS_FILE`), while `DirEntry::metadata` is
/// the `FindNextFile` record. An entry whose facts cannot be read is kept. The listener probe
/// is the fail-safe [`socket_is_live`]; where every connect fails `WSAEINVAL` (a
/// OneDrive-managed `%LOCALAPPDATA%\aterm`) only the dead-pid rule removes anything.
///
/// `keep_alias_target`: exempt the socket the `latest` alias ([`SOCK_FILE`]) names, as
/// [`sweep_stale_instances`] does. False for a legacy directory, where nothing will ever
/// repoint the alias.
#[cfg(any(windows, test))]
pub(crate) fn sweep_stale_socket_files(
    dir: &Path,
    min_age: std::time::Duration,
    keep_alias_target: bool,
) -> Option<SockSweep> {
    let entries = std::fs::read_dir(dir).ok()?;
    let aliased = keep_alias_target
        .then(|| aterm_uds::latest::target_name(&dir.join(SOCK_FILE)))
        .flatten()
        .and_then(|target| target.into_string().ok());
    let now = std::time::SystemTime::now();
    let facts: Vec<(String, u64, u64)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let meta = e.metadata().ok()?;
            // A last-write time in the future reads as age 0: kept, never guessed.
            let age_secs = meta
                .modified()
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .map_or(0, |d| d.as_secs());
            Some((name, meta.len(), age_secs))
        })
        .collect();
    let listing: Vec<control_socket::SockFileFacts<'_>> = facts
        .iter()
        .map(|(name, len, age_secs)| control_socket::SockFileFacts {
            name: name.as_str(),
            len: *len,
            age_secs: *age_secs,
        })
        .collect();
    let listening = |name: &str| socket_is_live(&dir.join(name).to_string_lossy());
    let mut report = SockSweep::default();
    for stale in control_socket::stale_socket_files(
        &listing,
        min_age.as_secs(),
        aliased.as_deref(),
        &imp::pid_alive,
        &listening,
    ) {
        if std::fs::remove_file(dir.join(&stale)).is_ok() {
            report.removed.push(stale);
        } else {
            report.failed.push(stale);
        }
    }
    Some(report)
}

/// The launch sweep (Windows only): [`sweep_stale_socket_files`] over the current socket
/// directory and the legacy one ([`imp::legacy_socket_dirs`]), one log line per directory
/// (INFO naming what went or would not go, DEBUG when nothing was stale). Runs beside
/// [`sweep_stale_instances`] at bind time, before this instance's socket exists, and covers
/// what the dead-pid rule cannot: a recycled pid, a socket name with no pid, the legacy
/// directory.
#[cfg(windows)]
pub(crate) fn sweep_stale_socket_files_at_launch(sock_dir: &Path) {
    // (directory, whether instances bind there — see `keep_alias_target`).
    let mut dirs = vec![(sock_dir.to_path_buf(), true)];
    for legacy in imp::legacy_socket_dirs() {
        if !dirs.iter().any(|(dir, _)| *dir == legacy) {
            dirs.push((legacy, false));
        }
    }
    for (dir, keep_alias_target) in dirs {
        let Some(report) = sweep_stale_socket_files(&dir, STALE_SOCK_MIN_AGE, keep_alias_target)
        else {
            continue;
        };
        if report.removed.is_empty() && report.failed.is_empty() {
            aterm_log::debug!(
                "control socket sweep: {}: no stale socket file",
                dir.display()
            );
            continue;
        }
        let failed = if report.failed.is_empty() {
            String::new()
        } else {
            format!("; could not remove: {}", report.failed.join(", "))
        };
        aterm_log::info!(
            "control socket sweep: {}: removed {} zero-length socket file(s) older than a day \
             with no listener: {}{failed}",
            dir.display(),
            report.removed.len(),
            report.removed.join(", ")
        );
    }
}

/// Graceful-exit cleanup off Unix: remove this instance's socket and token by path, and the
/// `latest` alias only while it still points at our socket. Crash exits are covered by
/// [`sweep_stale_instances`] at the next spawn. Unix removes by file identity instead
/// (`owned_endpoint`); the rule is plain `std`, so its test runs on Unix too.
#[cfg(any(not(unix), test))]
pub(crate) fn cleanup_socket(plan: &SocketPlan) {
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
/// token file and `images/` subdir for an explicit `--control-sock`.
#[must_use]
pub(crate) fn dir_of_socket(path: &str) -> PathBuf {
    Path::new(path)
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// The capability-token file beside the socket at `path`: the shared name rule
/// ([`control_socket::token_name_for_sock`]) resolved in that socket's directory, so server
/// and client cannot drift. Per socket, not per directory: two private instances in one
/// directory keep their own credentials.
#[must_use]
pub(crate) fn token_path_for_socket(path: &str) -> PathBuf {
    let name = Path::new(path)
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    dir_of_socket(path).join(control_socket::token_name_for_sock(&name))
}

/// Read back the trimmed capability token of the socket at `sock_path` through
/// [`token_path_for_socket`], the writer's rule; `None` if unreadable or empty.
#[cfg(test)]
#[must_use]
pub(crate) fn read_token(sock_path: &str) -> Option<String> {
    let raw = std::fs::read_to_string(token_path_for_socket(sock_path)).ok()?;
    let t = raw.trim().to_string();
    if t.is_empty() { None } else { Some(t) }
}

/// Whether a control socket at `path` has a live listener. Fail-safe: only connection-refused
/// or not-found means stale; any other error counts as live, so a maybe-live socket is never
/// unlinked (the Windows `CtlStream::connect` normalizes a missing path to `NotFound`).
#[must_use]
pub(crate) fn socket_is_live(path: &str) -> bool {
    use std::io::ErrorKind;
    match CtlStream::connect(path) {
        Ok(_) => true,
        Err(e) => !matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound),
    }
}

/// A bounded connection attempt for the incoming handoff's teardown wait.
/// A successful dial retains its kernel peer identity; the caller must refuse
/// an unrelated owner before retrying (repeated dials can fill its backlog).
#[cfg(unix)]
pub(crate) fn connect_socket_nonblocking(path: &str) -> std::io::Result<CtlStream> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let bytes = path.as_bytes();
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid socket path",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    // Close-on-exec at creation where the platform can: this runs on the control worker
    // while other threads spawn helpers, and a child spawned by a path that does not strip
    // inherited descriptors holds every unflagged one for its whole life. Darwin has no
    // `SOCK_CLOEXEC`, so there the `fcntl` pair below is the best available.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let kind = libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let kind = libc::SOCK_STREAM;
    // SAFETY: socket returns a fresh descriptor, owned immediately on success.
    let raw = unsafe { libc::socket(libc::AF_UNIX, kind, 0) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: both operations target this live, exclusively owned descriptor.
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0
    {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: initialized sockaddr_un with a checked, NUL-terminated path.
    let result = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            std::ptr::from_ref(&address).cast(),
            std::mem::size_of_val(&address) as libc::socklen_t,
        )
    };
    if result == 0 {
        Ok(CtlStream::from(fd))
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
pub(crate) fn connect_socket_nonblocking(path: &str) -> std::io::Result<CtlStream> {
    CtlStream::connect(path)
}

/// [`socket_is_live`] for the per-instance default plan (`aterm-<ourpid>.sock`), where the
/// fail-safe inverts: nobody else can own a path named after our pid, so an unexpected
/// connect error (`EPERM` under a hardened runtime, `ETIMEDOUT`, `ECONNRESET` from a crashed
/// same-pid predecessor's leftover) means stale junk, and treating it as live would run
/// socketless for the process lifetime. Only a successful connect refuses the bind here;
/// explicit `--control-sock` paths keep the strict [`socket_is_live`].
#[must_use]
pub(crate) fn socket_is_live_per_instance(path: &str) -> bool {
    CtlStream::connect(path).is_ok()
}

/// What to do with an existing socket path before binding.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BindAction {
    /// No live listener — remove any stale file and bind.
    RemoveAndBind,
    /// A live listener already owns this path: leave it and run socket-less.
    RefuseLiveSocket,
}

/// Decide whether to unlink and bind a socket path. A live socket is refused, so a nested
/// aterm that still sees an explicit `--control-sock` can never unlink and steal its parent's
/// listener (a second guard behind the environment deny-list).
#[must_use]
pub(crate) fn decide_bind(is_live: bool) -> BindAction {
    if is_live {
        BindAction::RefuseLiveSocket
    } else {
        BindAction::RemoveAndBind
    }
}

/// Outcome of parsing/validating a connection's first line against the token.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AuthOutcome {
    /// Authenticated. If the first line carried an inline `TOKEN <hex> <verb>`
    /// the remaining verb text is returned so the caller dispatches it now;
    /// a bare `AUTH <hex>` yields `None` (the next line is the first verb).
    Ok(Option<String>),
    /// Authentication failed (bad/missing token line, wrong token).
    Denied,
}

/// THE HARNESS TOKEN (design §2.5, phase 3A): 32 random bytes in hex, minted
/// on first use and held only in this process's memory — never written to a
/// file, the environment or a log — which the in-GUI harness presents on its
/// own instance's socket so its acts read `by=harness` in the session log.
/// It is the instance token's power under another name
/// (`control::Principal::Harness`), so it grants nothing an in-session client
/// lacks. `None` when no entropy is available: the harness then presents the
/// instance token, and reads `by=ctl`.
pub(crate) fn harness_token() -> Option<&'static str> {
    static TOKEN: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    TOKEN.get_or_init(random_token_hex).as_deref()
}

/// [`check_auth_line`] against the [`harness_token`] (the same
/// constant-time compare); `Denied` when there is none.
#[must_use]
pub(crate) fn check_harness_line(line: &str) -> AuthOutcome {
    check_line_against(line, harness_token())
}

/// [`check_auth_line`] against `token`, or `Denied` when there is none.
fn check_line_against(line: &str, token: Option<&str>) -> AuthOutcome {
    match token {
        Some(token) => check_auth_line(line, token),
        None => AuthOutcome::Denied,
    }
}

/// Validate a connection's first line against the expected token.
///
/// Accepts two equivalent forms so a client can authenticate without an extra
/// round-trip:
/// * `AUTH <hex>`            — dedicated auth line; the verb follows on line 2.
/// * `TOKEN <hex> <verb...>` — token + first verb folded into one line.
///
/// The comparison is aterm-digest's `ct_eq` (astream-digest's, the workspace's one
/// constant-time compare): no data-dependent exit over the token's bytes, and a
/// presented token of another length refused on its length, which is no secret (the
/// token is a fixed-width hex string). Anything else (no token line, wrong token,
/// malformed) is [`AuthOutcome::Denied`].
#[must_use]
pub(crate) fn check_auth_line(line: &str, expected: &str) -> AuthOutcome {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let (head, rest) = match line.split_once(' ') {
        Some((h, r)) => (h, r),
        None => (line, ""),
    };
    match head {
        "AUTH" => {
            if aterm_digest::ct_eq(rest.trim_end().as_bytes(), expected.as_bytes()) {
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
            if aterm_digest::ct_eq(hex.trim_end().as_bytes(), expected.as_bytes()) {
                AuthOutcome::Ok(Some(verb.to_string()))
            } else {
                AuthOutcome::Denied
            }
        }
        _ => AuthOutcome::Denied,
    }
}

/// A caller-supplied `image` path confined to the `images/` subdir, as a
/// canonical directory plus a single final filename component.
///
/// TOCTOU-1: confinement is decided on the control thread but the write happens on the main
/// thread, so the writer must not re-resolve a multi-segment path (an intermediate dir could
/// be swapped for a symlink in the gap). With nested target dirs forbidden, the writer opens
/// the canonical `images/` dir once and creates the bare filename relative to it.
#[derive(Clone, Debug)]
pub(crate) struct ConfinedImage {
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

    /// The full path, for logging / `OK <w> <h> <path>` replies only, not for
    /// re-opening (the writer must use [`Self::dir`] + [`Self::file_name`]).
    #[must_use]
    pub(crate) fn display_path(&self) -> PathBuf {
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
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
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
pub(crate) fn confine_image_path(sock_dir: &Path, requested: &str) -> Option<ConfinedImage> {
    confine_image_path_with_admission(sock_dir, requested, |_| true).ok()
}

/// Resolve a caller-supplied `image` path inside the socket directory's `images/` subdir,
/// independently of later resource admission.
///
/// The subdir is created `0700`. A relative or bare-filename request resolves into it; an
/// absolute one must already be inside it. Nested target directories are forbidden, so the
/// only directory component is the canonical subdir (TOCTOU-1). `None` when the request
/// would escape or names a nested path.
///
/// SPEC: this is the real `Confine` action of `aterm_models_gui::path_confine_model`
/// (`PathConfine`, the control-socket CONFINEMENT family). Its `WriteWithinSubdir` /
/// `EscapeRejected` invariants — a committed write only ever lands INSIDE the root, and a
/// request whose final component resolves OUTSIDE is rejected with no write — are exactly
/// the confused-deputy symlink escape this canonicalize-the-RESOLVED-location check fixes
/// (the old guard checked only the parent prefix, then the writer FOLLOWED a symlinked
/// last segment). A `Some(_)` here maps to the model's `committed=1, target=1` (inside); a
/// `None` (escape) maps to `committed=0, target=0` (none). Tier-1 conformance drives this
/// against a real planted symlink (`crates/aterm-gui/src/path_confine_conformance_tests.rs`).
// The anchor expands in every test and debug build, so aterm-gui's own closure gate sees
// it, and so does aterm-models' from outside the crate.
// PROJECTION (TRUST_VACUITY_GATE §2.2 / finding 2): `Confine` projects the real
// confine outcome onto the model's `<<decided, committed, target>>` — the projection
// the `path_confine_conformance_tests` Tier-1 test drives (`Some` → committed inside,
// `None` → rejected). L2 requires the projection NAME be present (Trust does
// not execute it); `aterm_gui::control_auth::project_confine` is that witness.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
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

    // Lexical normalization refuses a `..` above the root without touching the disk, so a
    // non-existent escape parent (`../../etc/passwd`) cannot slip past a canonicalize that
    // errors.
    let lexical = lexically_normalize(&raw_candidate)?;

    // Nested target dirs are forbidden (TOCTOU-1): the file's canonical parent must be
    // exactly the canonical images subdir.
    let file_name = lexical.file_name()?;
    // A filename with a path separator or `..`/`.` is not a single component.
    if !crate::pinned_dir::is_one_component(file_name) {
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
    // Reject a symlink at the final component up front: the writer also uses `O_NOFOLLOW`,
    // but this gives a clean `ERR path` without attempting the open.
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

/// Confine a discovery-graph socket path to the socket directory before the proxy forward
/// dials it and presents a parent-minted edge token. The graph entry
/// (`<sock_dir>/graph/<sid>`) is same-uid writable and carries the launch nonce in plain
/// text, so the nonce alone cannot stop a same-uid process rewriting it to
/// `sock <attacker-path>`. As for [`confine_image_path_with_admission`], the path must name a
/// real, non-symlink file directly in `sock_dir`; `None` fails the forward closed. The
/// publisher writes exactly `<sock_dir>/aterm-<pid>.sock`.
#[must_use]
pub(crate) fn confine_proxy_sock(sock_dir: &Path, requested: &str) -> Option<String> {
    let canon_dir = std::fs::canonicalize(sock_dir).ok()?;
    let req = Path::new(requested);
    if !req.is_absolute() {
        return None; // graph entries publish absolute instance-socket paths
    }
    // Normalize lexically first (a non-existent escape cannot slip past a canonicalize that
    // errors), then require one filename whose canonical parent is exactly our socket dir.
    let lexical = lexically_normalize(req)?;
    let file_name = lexical.file_name()?;
    if !crate::pinned_dir::is_one_component(file_name) {
        return None;
    }
    let canon_parent = std::fs::canonicalize(lexical.parent()?).ok()?;
    if canon_parent != canon_dir {
        return None;
    }
    // Reject a symlink at the final component: `connect()` follows symlinks, so a same-uid
    // attacker could otherwise plant `<sock_dir>/aterm-x.sock -> /attacker`.
    let resolved = canon_dir.join(file_name);
    if let Ok(md) = std::fs::symlink_metadata(&resolved)
        && md.file_type().is_symlink()
    {
        return None;
    }
    Some(resolved.to_string_lossy().into_owned())
}

/// Lexically resolve `.`/`..`/`//` in an absolute path without touching the filesystem.
/// `None` if a `..` would climb above the root. Symlinks are not followed: that is the
/// canonicalize re-check's job.
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
    use crate::native_document_host::HeldAdvisoryLock;

    #[test]
    fn client_named_paths_are_named_only_for_server_minted_ids() {
        let sock = Path::new("/c");
        assert_eq!(
            video_instance_root_for(sock, "p12-ab-3"),
            Some(sock.join(VIDEO_DIR).join("p12-ab-3"))
        );
        for bad in ["", ".", "..", "a/b", "a.b", "a\0b"] {
            assert_eq!(video_instance_root_for(sock, bad), None, "{bad:?}");
        }

        let dir = Path::new("/c/integration");
        let sid = aterm_session::SessionId::new("s-09035e075d6ee9388965".to_string());
        assert_eq!(
            sid_file_in(dir, &sid),
            Some(dir.join("s-09035e075d6ee9388965"))
        );
        for bad in ["../x", "s-", "s-zz", "s-12/../../etc", "abc"] {
            assert_eq!(
                sid_file_in(dir, &aterm_session::SessionId::new(bad.to_string())),
                None,
                "{bad}"
            );
        }
    }

    /// `socket_is_live` is true for a bound listener and false for an absent or stale socket,
    /// so the bind gate removes stale files but never unlinks a live listener. Alone in its
    /// own process: `drop(listener)` stops listening only with its last descriptor, and a
    /// sibling test's fork holds a copy until it execs (macOS honors no `FD_CLOFORK`).
    #[test]
    fn socket_is_live_true_for_bound_false_for_stale() {
        if !crate::test_alone::alone() {
            return;
        }
        let dir = crate::test_scratch::short_dir("the probed socket");
        let path = dir.path().join("s.sock");
        let ps = path.to_string_lossy().into_owned();
        assert!(!socket_is_live(&ps), "absent socket is not live");
        let listener = aterm_uds::CtlListener::bind(&path).expect("bind");
        assert!(socket_is_live(&ps), "bound socket is live");
        drop(listener); // file remains, nobody listens -> becomes stale
        // A transient connect error (EMFILE, EINTR) reads as maybe-live (the probe is
        // fail-safe), so poll with a hang detector.
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "stale socket file (no listener) becomes not-live",
            || (!socket_is_live(&ps)).then_some(()),
        );
    }

    /// A liveness probe must remain bounded even when nobody drains accept().
    /// A blocking connect can wait for this listener's queue to drain.
    #[cfg(unix)]
    #[test]
    fn socket_teardown_wait_does_not_block_on_a_full_accept_backlog() {
        use std::os::fd::AsRawFd;
        let dir = crate::test_scratch::short_dir("the control socket");
        let path = dir.path().join("backlog.sock");
        let listener = aterm_uds::CtlListener::bind(&path).unwrap();
        // SAFETY: lower the backlog of this test's own live listener.
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 1) }, 0);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let path = path.to_str().unwrap().to_string();
        let probe = std::thread::spawn(move || {
            let initially_live = connect_socket_nonblocking(&path).is_ok();
            for _ in 0..128 {
                // Only boundedness is promised here: a full backlog's errno
                // is platform-dependent. The real binder still probes itself.
                let _ = connect_socket_nonblocking(&path);
            }
            let _ = tx.send(initially_live);
        });
        // The answer must arrive: a hang detector, not a latency budget.
        let result = rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        // Close before asserting: this also releases a regressed blocking dial.
        drop(listener);
        probe.join().unwrap();
        assert_eq!(
            result,
            Ok(true),
            "teardown hint blocked or missed the initial live listener"
        );
    }

    /// The teardown dial is close-on-exec and non-blocking: a post-condition pin, not a test
    /// of atomicity (Linux flags the socket inside `socket(2)`; Darwin, with no
    /// `SOCK_CLOEXEC`, keeps the `fcntl` pair and its microsecond window).
    #[cfg(unix)]
    #[test]
    fn the_teardown_dial_is_close_on_exec_and_non_blocking() {
        use std::os::fd::AsRawFd;
        let dir = crate::test_scratch::short_dir("the control socket");
        let path = dir.path().join("cloexec.sock");
        let listener = aterm_uds::CtlListener::bind(&path).unwrap();
        let stream = connect_socket_nonblocking(path.to_str().unwrap()).unwrap();

        // SAFETY: both reads target this test's own live descriptor.
        let fd_flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFD) };
        let status_flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) };
        assert!(
            fd_flags >= 0 && fd_flags & libc::FD_CLOEXEC != 0,
            "the dial is inherited across exec: F_GETFD = {fd_flags:#x}"
        );
        assert!(
            status_flags >= 0 && status_flags & libc::O_NONBLOCK != 0,
            "the dial blocks: F_GETFL = {status_flags:#x}"
        );
        drop(stream);

        // A path no socket address can carry is refused before any dial: one
        // with a NUL (the kernel would dial the prefix — the live socket here)
        // and one that leaves `sun_path` no room for its terminator. The
        // longest path a plan binds is dialed (nobody is there).
        for (bad, kind) in [
            (
                format!("{}\0junk", path.to_str().unwrap()),
                std::io::ErrorKind::InvalidInput,
            ),
            (
                "a".repeat(MAX_SUN_PATH + 1),
                std::io::ErrorKind::InvalidInput,
            ),
            ("a".repeat(MAX_SUN_PATH), std::io::ErrorKind::NotFound),
        ] {
            let refused = connect_socket_nonblocking(&bad).expect_err("no listener");
            assert_eq!(refused.kind(), kind, "{bad:?}");
        }

        // The bind gate: a path a listener answers on is never unlinked —
        // under either probe — and an absent one is bound.
        let live = path.to_str().unwrap();
        let absent = dir.path().join("absent.sock");
        let absent = absent.to_str().unwrap();
        for (is_live, want) in [
            (socket_is_live(live), BindAction::RefuseLiveSocket),
            (
                socket_is_live_per_instance(live),
                BindAction::RefuseLiveSocket,
            ),
            (socket_is_live(absent), BindAction::RemoveAndBind),
            (
                socket_is_live_per_instance(absent),
                BindAction::RemoveAndBind,
            ),
        ] {
            assert_eq!(decide_bind(is_live), want);
        }
        drop(listener);
    }

    /// The `sun_path` limit is counted in bytes; an explicit path over it is refused by name
    /// ([`SocketResolution::PathTooLong`]) before any bind or probe.
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

    /// Each way the launch can end up socketless is its own named outcome,
    /// and each bindable plan pairs the socket with its own token.
    #[test]
    fn the_socket_plan_names_every_refusal() {
        use SocketDirective::{Disabled, Explicit, PerInstance};
        let temp = crate::test_scratch::short_dir("the explicit socket");
        let short = temp.path().join("a.sock").to_string_lossy().into_owned();
        let SocketResolution::Enabled(plan) =
            resolve_socket_plan_for(Explicit(short.clone()), || None)
        else {
            panic!("a short explicit path binds");
        };
        assert_eq!(
            (plan.sock_path.as_str(), plan.latest_link),
            (short.as_str(), None)
        );
        assert_eq!(plan.token_path, token_path_for_socket(&short));
        let long = format!("/{}", "a".repeat(MAX_SUN_PATH));
        assert!(matches!(
            resolve_socket_plan_for(Explicit(long.clone()), || None),
            SocketResolution::PathTooLong { path, limit } if path == long && limit == MAX_SUN_PATH
        ));
        assert!(matches!(
            resolve_socket_plan_for(Disabled, || None),
            SocketResolution::Disabled
        ));
        assert!(matches!(
            resolve_socket_plan_for(PerInstance, || None),
            SocketResolution::NoDir
        ));
        let not_a_dir = temp.path().join("file");
        std::fs::write(&not_a_dir, b"x").unwrap();
        assert!(matches!(
            resolve_socket_plan_for(PerInstance, || Some(not_a_dir.clone())),
            SocketResolution::DirUnusable { dir, .. } if dir == not_a_dir
        ));
        let dir = temp.path().join("control");
        let SocketResolution::Enabled(plan) =
            resolve_socket_plan_for(PerInstance, || Some(dir.clone()))
        else {
            panic!("a private per-user directory binds");
        };
        let pid = std::process::id();
        assert_eq!(
            plan.sock_path,
            dir.join(control_socket::instance_sock_name(pid))
                .to_string_lossy()
        );
        assert_eq!(
            plan.token_path,
            dir.join(control_socket::instance_token_name(pid))
        );
        assert_eq!(plan.latest_link, Some(dir.join(SOCK_FILE)));
    }

    /// The best-effort cleanup queue admits exactly its limit
    /// ([`crate::control::ARTIFACT_CLEANUP_QUEUE_LIMIT`]), a permit's clones (a
    /// panic's recovery copy) hold one place between them, and the place is
    /// admitted again once the last of them drops. The budgets are this test's
    /// own ([`crate::control::ArtifactBudgets::shipping`]), so the queue starts
    /// empty.
    #[test]
    fn the_cleanup_queue_admits_exactly_its_limit_and_a_freed_place_again() {
        let budgets = crate::control::ArtifactBudgets::shipping();
        let reserve = || ArtifactCleanupQueuePermit::try_reserve(&budgets);
        let mut permits: Vec<_> = (0..crate::control::ARTIFACT_CLEANUP_QUEUE_LIMIT)
            .map(|_| reserve().expect("within the limit"))
            .collect();
        assert!(reserve().is_none(), "one past the limit is refused");
        let held = permits.pop().expect("an admitted place");
        let recovery = held.clone();
        drop(held);
        assert!(
            reserve().is_none(),
            "the recovery copy still holds the place"
        );
        drop(recovery);
        permits.push(reserve().expect("a freed place"));
        assert!(reserve().is_none());
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

    /// `AUTH <token>` and `TOKEN <token> [verb]` admit the exact token only
    /// (a trailing CR tolerated); anything else is denied.
    #[test]
    fn auth_line_admits_only_the_exact_token() {
        let tok = "a".repeat(64);
        let bad = "b".repeat(64);
        for (line, want) in [
            (format!("AUTH {tok}"), AuthOutcome::Ok(None)),
            (format!("AUTH {tok}\r"), AuthOutcome::Ok(None)),
            (format!("AUTH {tok} "), AuthOutcome::Ok(None)),
            (format!("AUTH {bad}"), AuthOutcome::Denied),
            ("AUTH".to_string(), AuthOutcome::Denied),
            ("text".to_string(), AuthOutcome::Denied),
            (String::new(), AuthOutcome::Denied),
            (
                format!("TOKEN {tok} text"),
                AuthOutcome::Ok(Some("text".to_string())),
            ),
            (
                format!("TOKEN {tok} send echo hi"),
                AuthOutcome::Ok(Some("send echo hi".to_string())),
            ),
            (format!("TOKEN {tok}"), AuthOutcome::Ok(Some(String::new()))),
            (format!("TOKEN {bad} text"), AuthOutcome::Denied),
            // The CR goes before the verb is split off, not into the verb.
            (
                format!("TOKEN {tok} text\r"),
                AuthOutcome::Ok(Some("text".to_string())),
            ),
            // Exactly one space before the token, and the whole token.
            (format!("AUTH  {tok}"), AuthOutcome::Denied),
            ("TOKEN".to_string(), AuthOutcome::Denied),
            ("TOKEN  text".to_string(), AuthOutcome::Denied),
            (format!("TOKEN {} text", &tok[..63]), AuthOutcome::Denied),
            // The token followed by 65,536 NUL bytes, which a line can carry:
            // refused on its length, whatever the delta and whatever lies past
            // the token. A compare that folded only the delta's low 16 bits
            // and read a zero past the shorter side would admit both (the
            // delta folds to 0, and no byte differs). The compare is
            // astream-digest's ct_eq, whose own `ct_eq_rejects_length_mismatch`
            // holds all-zero slices 65,536 bytes apart unequal in both orders;
            // these hold that no such fold stands between it and the line.
            (
                format!("AUTH {tok}{}", "\0".repeat(65_536)),
                AuthOutcome::Denied,
            ),
            (
                format!("TOKEN {tok}{} text", "\0".repeat(65_536)),
                AuthOutcome::Denied,
            ),
        ] {
            assert_eq!(check_auth_line(&line, &tok), want, "{line:?}");
        }
        // The harness's door: its own token only, and nothing at all when
        // there is none (no entropy at startup).
        let line = format!("AUTH {tok}");
        assert_eq!(check_line_against(&line, Some(&tok)), AuthOutcome::Ok(None));
        assert_eq!(check_line_against(&line, Some(&bad)), AuthOutcome::Denied);
        assert_eq!(check_line_against(&line, None), AuthOutcome::Denied);
        assert_eq!(check_line_against("AUTH ", None), AuthOutcome::Denied);
    }

    /// SEC-3: our own loosened directory is forced back to exactly `0700`
    /// (special bits too), and a directory that is not ours is REFUSED, even
    /// one already `0700`: a chmod cannot change its owner.
    #[test]
    #[cfg(unix)]
    fn ensure_private_dir_hardens_ours_and_refuses_a_strangers() {
        use std::os::unix::fs::PermissionsExt;
        let temp = aterm_tempfile::tempdir().unwrap();
        let dir = temp.path().join("control");
        let mode = || std::fs::metadata(&dir).unwrap().permissions().mode() & 0o7777;
        ensure_private_dir(&dir).unwrap();
        for loose in [0o770, 0o1700] {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(loose)).unwrap();
            ensure_private_dir(&dir).unwrap();
            assert_eq!(mode(), 0o700, "{loose:o} is forced to 0700");
        }
        // Standing as another uid is the one unprivileged way to own nothing.
        let stranger = our_uid().wrapping_add(1);
        for planted in [0o700, 0o755] {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(planted)).unwrap();
            let refused = imp::ensure_private_dir_as(&dir, stranger)
                .expect_err("a directory owned by another uid is never private");
            assert_eq!(
                refused.kind(),
                std::io::ErrorKind::PermissionDenied,
                "{planted:o}"
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn provision_token_refuses_symlinked_path() {
        // SEC-3: provision_token never writes through a symlink planted at the token path.
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("aterm-tok-sym-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"original").unwrap();
        let tokpath = dir.join("aterm.token");
        symlink(&victim, &tokpath).unwrap();
        // Unlink-first removes the symlink; the token lands in a fresh regular file.
        let _ = provision_token(&tokpath);
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"original",
            "the symlink target must not be written through",
        );
        // The token path is now a regular file.
        let md = std::fs::symlink_metadata(&tokpath).unwrap();
        assert!(
            md.file_type().is_file(),
            "token path must be a regular file"
        );
        // What a racer plants in the gap after the unlink — a hard link to a
        // file of ours — is refused by the exclusive create, not truncated.
        std::fs::remove_file(&tokpath).unwrap();
        std::fs::hard_link(&victim, &tokpath).unwrap();
        assert_eq!(imp::write_fresh_token(&tokpath, "token"), None);
        assert_eq!(std::fs::read(&victim).unwrap(), b"original");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn provision_and_read_token_roundtrip() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("aterm-auth-test-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        // Write and read through the shared name rule, as the server and client do.
        let sock = dir.join("a.sock").to_string_lossy().into_owned();
        let token_path = token_path_for_socket(&sock);
        assert_eq!(token_path, dir.join("a.sock.token"));
        let written = provision_token(&token_path).expect("token written");
        let read = read_token(&sock).expect("token readable");
        assert_eq!(written, read);
        // A second socket in the same directory has its own token file, and provisioning
        // it leaves the first one's alone.
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

    /// Windows twin of the Unix roundtrip and symlink tests: `create_new` refuses any
    /// pre-existing object at the path, and `provision_token`'s unlink-first still lets a
    /// rotation replace our own stale token.
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

        // An absolute path inside the subdir is allowed.
        let inside = dir.join(IMAGES_DIR).join("ok.png");
        let allowed =
            confine_image_path(&dir, inside.to_str().unwrap()).expect("absolute-inside allowed");
        assert!(allowed.display_path().ends_with("images/ok.png"));

        // The private namespaces under images/ are no caller's file name, nor is
        // a name with a NUL, nor a `..` above the root, even one that lands inside.
        let above_root = format!(
            "/..{}",
            std::fs::canonicalize(dir.join(IMAGES_DIR))
                .unwrap()
                .join("ok.png")
                .display()
        );
        for refused in [
            AUTO_IMAGES_DIR,
            CAPTURE_LOCK_DIR,
            "a\0b.png",
            above_root.as_str(),
        ] {
            assert!(confine_image_path(&dir, refused).is_none(), "{refused:?}");
        }
        // A `..` that would climb above the root refuses the path, never
        // dropping the step.
        for (path, want) in [
            ("/a/./b/../c", Some("/a/c")),
            ("/..", None),
            ("/a/../../b", None),
        ] {
            assert_eq!(
                lexically_normalize(Path::new(path)),
                want.map(PathBuf::from),
                "{path}"
            );
        }

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
        // TOCTOU-1: a nested target dir is forbidden even inside images/, so the writer
        // only opens the canonical images/ dir and creates one name in it.
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
        // A token-holding client plants a symlink at the final component (images/evil.png
        // -> a file outside): the parent canonicalizes inside images/, and a writer
        // following the link would clobber an arbitrary file.
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
        // Legit cases still work: a fresh name and an existing regular file.
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
        // A server-chosen stem is one plain word: nothing else is minted.
        for stem in ["", "a/b", "a.b"] {
            assert!(
                confine_automatic_image_path(&dir, stem).is_none(),
                "{stem:?}"
            );
        }

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
        let captures = || {
            std::fs::read_dir(&auto_dir)
                .unwrap()
                .flatten()
                .filter_map(|entry| {
                    Some((
                        automatic_capture_sequence(&entry.file_name())?,
                        entry.path(),
                    ))
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        assert_eq!(
            captures().len(),
            AUTO_IMAGE_KEEP,
            "successful auto captures converge to the retention cap"
        );
        // A capture the sweep cannot remove is no removal: the next oldest goes.
        #[cfg(target_os = "macos")]
        {
            let held = captures().into_values().collect::<Vec<_>>();
            let _undeletable = Immutable::set(&held[0]);
            let target = confine_automatic_image_path(&dir, "image").unwrap();
            std::fs::write(target.display_path(), b"png").unwrap();
            prune_automatic_image_dir(&target);
            let left = captures().into_values().collect::<Vec<_>>();
            assert_eq!(left.len(), AUTO_IMAGE_KEEP);
            assert!(left.contains(&held[0]) && !left.contains(&held[1]));
        }
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
        // Not even one shaped like a capture, in a socket directory named `auto`.
        let auto_sock = dir.join(AUTO_IMAGES_DIR);
        ensure_private_dir(&auto_sock).unwrap();
        let lookalike = confine_image_path(
            &auto_sock,
            &automatic_capture_name_for("image", process_instance_id(), 1),
        )
        .unwrap();
        std::fs::write(lookalike.display_path(), b"explicit").unwrap();
        cleanup_failed_automatic_image(&lookalike);
        assert!(lookalike.display_path().is_file());
        // An explicit name stays the caller's to reuse: a capture to it again
        // replaces it. A minted name is never anything else's: a file found
        // already at it is refused, never replaced.
        drop(explicit.write_private(b"again").unwrap());
        assert_eq!(std::fs::read(explicit.display_path()).unwrap(), b"again");
        let minted = confine_automatic_image_path(&dir, "image").unwrap();
        std::fs::write(minted.display_path(), b"planted").unwrap();
        let refused = minted.write_private(b"png").unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(minted.display_path()).unwrap(), b"planted");
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
        // The waiter's bound is a hang detector: the release below comes whenever this
        // thread runs again.
        let waiter = std::thread::spawn(move || {
            acquire_capture_name_lease_with_wait(
                &second,
                || {
                    worker_observed_wait.store(true, std::sync::atomic::Ordering::Release);
                    false
                },
                aterm_spec::wait::MUST_HAPPEN,
            )
        });
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the successor observes the retained reply lease",
            || {
                observed_wait
                    .load(std::sync::atomic::Ordering::Acquire)
                    .then_some(())
            },
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
        // A hang detector, as in the waiter test above: the cancellation
        // must win whenever this thread gets to set it.
        let waiter = std::thread::spawn(move || {
            acquire_capture_name_lease_with_wait(
                &second,
                || {
                    worker_observed_wait.store(true, std::sync::atomic::Ordering::Release);
                    worker_cancelled.load(std::sync::atomic::Ordering::Acquire)
                },
                aterm_spec::wait::MUST_HAPPEN,
            )
        });
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the cancellation probe reaches the wait",
            || {
                observed_wait
                    .load(std::sync::atomic::Ordering::Acquire)
                    .then_some(())
            },
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

        // A cancellation that lands once the namespace is reserved still wins,
        // and leaves the namespace free at once.
        let probes = std::cell::Cell::new(0);
        let late = acquire_capture_name_lease(&first, || {
            probes.set(probes.get() + 1);
            probes.get() > 1
        });
        assert!(late.expect("cancellation is not an I/O failure").is_none());
        let image_lease =
            acquire_capture_name_lease_with_wait(&first, || false, std::time::Duration::ZERO)
                .expect("the cancelled reservation let its namespace go")
                .expect("explicit lease");
        // An image lease holds no video retention capability to arm.
        let unrelated = crate::pinned_dir::PinnedDir::open_resolved(&dir).unwrap();
        assert_eq!(
            image_lease
                .arm_video_retention_sweep(&unrelated)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        drop(image_lease);
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

    /// Another process's hold on the shared image namespace (its own open file description
    /// of the directory) gets the same bounded wait as this process's own, and past it the
    /// refusal says why.
    #[test]
    #[cfg(unix)]
    fn another_processs_namespace_lock_is_waited_for_then_named() {
        let dir =
            std::env::temp_dir().join(format!("aterm-img-sibling-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();
        let image = ConfinedImage::for_test(&dir, "sibling.png");
        let sibling = HeldAdvisoryLock::try_lock(
            image
                .pinned()
                .unwrap()
                .open_directory_lock()
                .expect("the sibling's own description of the directory"),
        )
        .expect("the sibling holds the namespace");
        let error = acquire_capture_name_lease_with_wait(
            &image,
            || false,
            std::time::Duration::from_millis(60),
        )
        .expect_err("a sibling that never lets go is reported, not waited on forever");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert!(
            error.to_string().contains("not acknowledged"),
            "the refusal names the cause: {error}"
        );
        // Released inside the wait, the capture queues and then proceeds. The sibling lets
        // go on a fact, not a clock: the capture asks its cancellation probe before it first
        // tries the lock and again each time it finds it held, so the second ask is a
        // capture already queued behind the sibling.
        let asks = std::sync::atomic::AtomicUsize::new(0);
        let lease = acquire_capture_name_lease_with_wait(
            &image,
            || {
                if asks.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 1 {
                    sibling.file().unlock().expect("the sibling lets go");
                }
                false
            },
            aterm_spec::wait::MUST_HAPPEN,
        )
        .expect("the queued capture proceeds once the sibling lets go")
        .expect("not cancelled");
        assert!(
            asks.load(std::sync::atomic::Ordering::Relaxed) >= 2,
            "the capture queued behind the sibling before it proceeded"
        );
        drop(lease);
        drop(sibling);
        drop(image);
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
            .file()
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
        // Retention's last word before it unlinks: a leased reply is never
        // touched, even when the census raced the lease.
        assert_eq!(mutate_unleased_artifact(&queued_path, || ()), None);

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

        // A namespace whose launch lease is a planted link cannot be leased, so
        // nothing is confined into it, and nothing is made through the link.
        let lease_sock =
            std::env::temp_dir().join(format!("aterm-media-roots-lease-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&lease_sock);
        ensure_private_dir(&lease_sock).unwrap();
        let video = ensure_canonical_direct_child(&lease_sock, VIDEO_DIR).unwrap();
        let images = ensure_canonical_direct_child(&lease_sock, IMAGES_DIR).unwrap();
        let auto = ensure_canonical_direct_child(&images, AUTO_IMAGES_DIR).unwrap();
        for namespace in [video.join("p1-planted"), auto.join(process_instance_id())] {
            std::fs::create_dir(&namespace).unwrap();
            symlink(outside.join("lease"), namespace.join(INSTANCE_LEASE_FILE)).unwrap();
        }
        assert!(confine_video_dir_for_instance(&lease_sock, "p1-planted").is_none());
        assert!(confine_automatic_image_path(&lease_sock, "image").is_none());
        assert!(!outside.join("lease").exists());

        let _ = std::fs::remove_dir_all(lease_sock);
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

        // The publisher's `<sock_dir>/aterm-<pid>.sock` is accepted, rooted at the canonical
        // dir so a later dial cannot be redirected through a symlinked prefix.
        let legit = dir.join("aterm-42.sock");
        let got =
            confine_proxy_sock(&dir, &legit.to_string_lossy()).expect("in-dir socket allowed");
        assert_eq!(got, canon.join("aterm-42.sock").to_string_lossy());

        // An absolute path outside the runtime dir (the attacker overwrite) is rejected.
        assert!(confine_proxy_sock(&dir, "/tmp/evil-attacker.sock").is_none());
        // A `..` escape is rejected.
        let escape = dir.join("../evil.sock");
        assert!(confine_proxy_sock(&dir, &escape.to_string_lossy()).is_none());
        // A relative path is refused even where this process's cwd would land
        // it in the dir: what it names depends on whose cwd reads it. A
        // subdirectory of the cwd stands in for the socket dir (the parallel
        // test runner never changes cwd).
        let cwd = std::env::current_dir().unwrap();
        let sub = std::fs::read_dir(&cwd)
            .unwrap()
            .flatten()
            .find(|entry| {
                entry.file_type().is_ok_and(|kind| kind.is_dir())
                    && entry.file_name().to_str().is_some()
            })
            .expect("the test's cwd has a subdirectory")
            .file_name();
        let relative = Path::new(&sub).join("aterm-42.sock");
        let in_cwd = cwd.join(&sub);
        assert!(confine_proxy_sock(&in_cwd, &cwd.join(&relative).to_string_lossy()).is_some());
        assert!(
            confine_proxy_sock(&in_cwd, relative.to_str().unwrap()).is_none(),
            "relative rejected"
        );
        // A nested subdir under the runtime dir is rejected (single component only),
        // and so is a name with a NUL in it.
        let nested = dir.join("sub/aterm-42.sock");
        assert!(confine_proxy_sock(&dir, &nested.to_string_lossy()).is_none());
        let nul = format!("{}/aterm\0.sock", dir.display());
        assert!(confine_proxy_sock(&dir, &nul).is_none());

        // A symlink planted at the final component is rejected: `connect()` would follow it.
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
        // Every Unix aterm builds for has a peer-cred primitive (getpeereid,
        // SO_PEERCRED); where one is missing peer_uid is None and the caller
        // fails closed, which this test would then name rather than pass over.
        assert_eq!(peer_uid(&a), Some(our_uid()));
        // And the accept gate accepts our own connection.
        assert_eq!(peer_check(&a), Ok(()));
        // The gate's decision: only our uid; another uid — root's too — or
        // none the kernel will name, is refused.
        assert_eq!(imp::peer_verdict(Some(501), 501), Ok(()));
        for stranger in [Some(502), Some(0), None] {
            assert!(imp::peer_verdict(stranger, 501).is_err(), "{stranger:?}");
        }
    }

    /// Windows: a same-process pair is the same user, so the pid gate
    /// (`SIO_AF_UNIX_GETPEERPID` + token-user compare) passes it — and where
    /// the ioctl exists it names this process: the negative control that the pid is the
    /// peer's and not a stand-in.
    #[test]
    #[cfg(windows)]
    fn peer_check_passes_a_same_user_peer_on_windows() {
        let (a, _b) = CtlStream::pair().expect("socketpair");
        assert_eq!(peer_check(&a), Ok(()));
        if let Ok(pid) = aterm_uds::fdpass::peer_pid(&a) {
            assert_eq!(pid, std::process::id());
        }
    }

    #[test]
    fn sweep_removes_only_dead_instances_files() {
        let dir = std::env::temp_dir().join(format!("aterm-sweep-test-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        // A certainly-dead pid: a reaped child cannot be signalled any more.
        #[cfg(unix)]
        let mut child = aterm_uds::spawnfd::command("/bin/sleep")
            .arg("0")
            .spawn()
            .unwrap();
        #[cfg(windows)]
        let mut child = aterm_uds::spawnfd::command("cmd")
            .args(["/c", "exit"])
            .spawn()
            .unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let us = std::process::id();
        assert!(pid_alive(us) && !pid_alive(dead));
        assert!(
            !pid_alive(0) && !pid_alive(u32::MAX),
            "a pid that cannot be real (0, wider than pid_t) is dead, never a group or broadcast"
        );
        // Wider than pid_t, it would name a process group: ours is real.
        #[cfg(unix)]
        {
            // SAFETY: getpgrp has no preconditions and cannot fail.
            let group = unsafe { libc::getpgrp() } as u32;
            assert!(!pid_alive(group.wrapping_neg()));
        }
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

        // The alias's target is never swept, dead or not: between a crash and the next bind
        // the alias names a dead instance's pair, which goes once the alias moves on.
        touch(&control_socket::instance_sock_name(dead));
        touch(&control_socket::instance_token_name(dead));
        let link = dir.join(SOCK_FILE);
        publish_latest_link(&link, &control_socket::instance_sock_name(dead));
        sweep_stale_instances(&dir);
        assert!(dir.join(control_socket::instance_sock_name(dead)).exists());
        assert!(dir.join(control_socket::instance_token_name(dead)).exists());
        publish_latest_link(&link, &control_socket::instance_sock_name(us));
        sweep_stale_instances(&dir);
        assert!(!dir.join(control_socket::instance_sock_name(dead)).exists());
        assert!(!dir.join(control_socket::instance_token_name(dead)).exists());
        assert!(dir.join(control_socket::instance_sock_name(us)).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The launch sweep against a real directory. With the day margin only the dead pid's
    /// zero-byte file goes; younger files (two real sockets among them) are not probed, and a
    /// token, a file with bytes and the `latest` alias are never candidates. With no margin
    /// the dropped listener's leftover goes and the bound listener survives every pass. The
    /// alias's target is kept only where a bind will move the alias on.
    ///
    /// Alone in its own process: the orphan's listener stops only with its last descriptor,
    /// and a sibling test's fork holds a copy until it execs.
    #[test]
    fn launch_sweep_removes_only_aged_empty_socks_nobody_listens_on() {
        use std::time::{Duration, SystemTime};
        if !crate::test_alone::alone() {
            return;
        }
        let scratch = crate::test_scratch::short_dir("the swept sockets");
        let dir = scratch.path().join("socks");
        ensure_private_dir(&dir).unwrap();
        // A certainly-dead pid: a reaped child cannot be signalled any more.
        #[cfg(unix)]
        let mut child = aterm_uds::spawnfd::command("/bin/sleep")
            .arg("0")
            .spawn()
            .unwrap();
        #[cfg(windows)]
        let mut child = aterm_uds::spawnfd::command("cmd")
            .args(["/c", "exit"])
            .spawn()
            .unwrap();
        let dead = child.id();
        child.wait().unwrap();
        // `try_exists`, not `exists`: on Windows a bound socket file is a
        // reparse point `metadata` cannot open (1920), which `exists` reads as
        // absent; `try_exists` reports it present.
        let present = |p: &Path| p.try_exists().unwrap_or(false);

        let two_days_ago =
            SystemTime::now() - Duration::from_secs(2 * control_socket::STALE_SOCK_MIN_AGE_SECS);
        let plant = |name: &str, body: &[u8], when: Option<SystemTime>| {
            let path = dir.join(name);
            std::fs::write(&path, body).unwrap();
            if let Some(when) = when {
                std::fs::File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(when)
                    .unwrap();
            }
        };
        let dead_sock = control_socket::instance_sock_name(dead);
        let dead_token = control_socket::instance_token_name(dead);
        plant(&dead_sock, b"", Some(two_days_ago)); // stale on the name alone
        plant(&dead_token, b"tok", Some(two_days_ago)); // not a .sock
        plant("young.sock", b"", None); // younger than the margin
        plant("pointer.sock", b"aterm-1.sock", Some(two_days_ago)); // bytes in it
        plant(control_socket::LATEST_SOCK_FILE, b"", Some(two_days_ago)); // the alias, by name
        // Two real socket files, judged on liveness alone below (a same-process
        // `set_modified` cannot open their reparse points on Windows, so they
        // stay young and the zero-margin pass is what probes them).
        let live_path = dir.join("live.sock");
        let live = aterm_uds::CtlListener::bind(&live_path).expect("bind live");
        let orphan_path = dir.join("orphan.sock");
        let orphan = aterm_uds::CtlListener::bind(&orphan_path).expect("bind orphan");
        drop(orphan); // file remains, nobody listens

        let report = sweep_stale_socket_files(&dir, STALE_SOCK_MIN_AGE, true).expect("dir listed");
        assert_eq!(report.removed, vec![dead_sock.clone()]);
        assert!(report.failed.is_empty());
        assert!(!present(&dir.join(&dead_sock)));
        assert!(present(&dir.join(&dead_token)));
        assert!(present(&dir.join("young.sock")));
        assert!(present(&dir.join("pointer.sock")));
        assert!(present(&dir.join(control_socket::LATEST_SOCK_FILE)));
        assert!(
            present(&live_path) && present(&orphan_path),
            "younger than the margin: neither real socket is probed or touched"
        );

        // No margin: the orphan goes once its listener is torn down (alone, at the first
        // pass). The passes are capped: each dials `live.sock`, which never accepts, so the
        // probes stay inside its backlog.
        let mut orphan_gone = false;
        for _ in 0..40 {
            let report = sweep_stale_socket_files(&dir, Duration::ZERO, true).expect("dir listed");
            assert!(
                !report.removed.iter().any(|n| n == "live.sock"),
                "a live listener is never swept"
            );
            if report.removed.iter().any(|n| n == "orphan.sock") {
                orphan_gone = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            orphan_gone,
            "a dropped listener's leftover must go once stale"
        );
        assert!(!present(&orphan_path));
        assert!(present(&live_path));
        assert!(
            socket_is_live(&live_path.to_string_lossy()),
            "still answering"
        );
        // Never candidates, at any margin.
        assert!(present(&dir.join(&dead_token)));
        assert!(present(&dir.join("pointer.sock")));
        assert!(present(&dir.join(control_socket::LATEST_SOCK_FILE)));

        // The socket the `latest` alias names, dead and a day old: kept where
        // instances bind (the alias moves on at the next bind, and the
        // dead-pid sweep keeps it too), swept in a legacy directory, where no
        // bind will ever repoint that alias.
        plant(&dead_sock, b"", Some(two_days_ago));
        std::fs::remove_file(dir.join(control_socket::LATEST_SOCK_FILE)).unwrap();
        publish_latest_link(&dir.join(SOCK_FILE), &dead_sock);
        let report = sweep_stale_socket_files(&dir, STALE_SOCK_MIN_AGE, true).expect("dir listed");
        assert!(report.removed.is_empty(), "{report:?}");
        assert!(present(&dir.join(&dead_sock)));
        let report = sweep_stale_socket_files(&dir, STALE_SOCK_MIN_AGE, false).expect("dir listed");
        assert_eq!(report.removed, vec![dead_sock.clone()]);
        assert!(!present(&dir.join(&dead_sock)));

        // An absent directory is `None`, not an empty report.
        assert!(sweep_stale_socket_files(&dir.join("absent"), Duration::ZERO, true).is_none());
        drop(live);
    }

    #[test]
    fn latest_link_publishes_atomically_and_repoints() {
        let dir = std::env::temp_dir().join(format!("aterm-link-test-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let link = dir.join(SOCK_FILE);

        let first = dir.join("aterm-101.sock");
        publish_latest_link(&link, first.to_str().unwrap());
        // The target is the relative instance name (valid via any dir path);
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

    /// Holds a path immutable (`chflags uchg`, as any same-uid actor can), so
    /// no unlink or rename of it succeeds until this drops, unwinding included.
    #[cfg(target_os = "macos")]
    struct Immutable(std::ffi::CString);

    #[cfg(target_os = "macos")]
    impl Immutable {
        fn set(path: &Path) -> Self {
            use std::os::unix::ffi::OsStrExt as _;
            let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: `path` is NUL-terminated and outlives the call.
            let set = unsafe { libc::chflags(path.as_ptr(), libc::UF_IMMUTABLE) };
            assert_eq!(set, 0, "{path:?}");
            Self(path)
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for Immutable {
        fn drop(&mut self) {
            // SAFETY: `self.0` is NUL-terminated and outlives the call.
            unsafe { libc::chflags(self.0.as_ptr(), 0) };
        }
    }

    /// Arm `window` on this thread: the next pass through it runs `swap`, once.
    /// Unix only: Windows will not rename a directory a handle holds open.
    #[cfg(unix)]
    fn race_at(window: RaceWindow, swap: impl FnOnce() + 'static) {
        ARMED_RACE.set(Some((window, Box::new(swap))));
    }

    /// Whether the swap [`race_at`] armed has run; disarms it either way.
    #[cfg(unix)]
    fn race_ran() -> bool {
        ARMED_RACE.take().is_none()
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
        // aterm-durable's counting seam, folded in here rather than minting one more
        // recording against the process-wide cleanup budget: the index's bytes and the
        // marker's are only ORDERED (the members land before the marker that makes them
        // visible, nothing depends on the marker after a power loss, and the requester
        // is local); the batch barrier and the marker's entry are the recording
        // directory's untouched plain `fsync(2)`, which records nothing. Windows' is
        // its counterpart `FlushFileBuffers`, which records nothing either
        // (`pinned_dir`'s `flush_directory`), so one expectation holds on both.
        let flushes = aterm_durable::seam::Recording::start();
        let index = producer
            .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
            .unwrap();
        assert_eq!(
            flushes.take(),
            [aterm_durable::Flush::Ordered],
            "the index's bytes"
        );
        producer.publish(&[], &index).unwrap();
        assert_eq!(
            flushes.take(),
            [],
            "the batch barrier: the directory's plain flush"
        );
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
        assert_eq!(
            flushes.take(),
            [aterm_durable::Flush::Ordered],
            "the marker's bytes"
        );
        drop(flushes);
        assert!(path.join(VIDEO_PUBLISHED_FILE).is_file());
        assert_eq!(
            artifact_lease_count(&path),
            Some(1),
            "publishing the marker does not release the producer lease"
        );

        let root = crate::pinned_dir::PinnedDir::open_resolved(path.parent().unwrap()).unwrap();
        let name = path.file_name().expect("recording name").to_os_string();
        let reader_recording = root.child(&name).unwrap();
        let reader = retain_video_artifact_path(
            root,
            name,
            &reader_recording,
            &crate::control::ArtifactBudgets::shipping(),
        )
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
        let reader = retain_video_artifact_path(
            reader_root,
            name,
            &reader_recording,
            &crate::control::ArtifactBudgets::shipping(),
        )
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
        let first = retain_video_artifact_path(
            first_root,
            fresh.clone(),
            &first_recording,
            &crate::control::ArtifactBudgets::shipping(),
        )
        .unwrap()
        .expect("first reader lease");
        let key = first.key.clone().expect("reader owns its canonical key");
        first.arm_video_retention_sweep(&first_recording).unwrap();
        drop(first_recording);

        let second_root = crate::pinned_dir::PinnedDir::open_resolved(&root_path).unwrap();
        let second_recording = second_root.child(&fresh).unwrap();
        let second = retain_video_artifact_path(
            second_root,
            fresh,
            &second_recording,
            &crate::control::ArtifactBudgets::shipping(),
        )
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
    fn video_lease_capabilities_bind_their_own_recording_and_root() {
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

        let other = std::ffi::OsString::from("rec-00000000000000000001-000");
        write_completed_video_fixture(&root_path, other.to_str().unwrap());

        let first_root = crate::pinned_dir::PinnedDir::open_resolved(&root_path).unwrap();
        let first_recording = first_root.child(&fresh).unwrap();
        let other_recording = first_root.child(&other).unwrap();
        let first = retain_video_artifact_path(
            first_root,
            fresh.clone(),
            &first_recording,
            &crate::control::ArtifactBudgets::shipping(),
        )
        .unwrap()
        .expect("first reader lease");

        // Another recording's handle is no capability for this one.
        let again = crate::pinned_dir::PinnedDir::open_resolved(&root_path).unwrap();
        let invalid = |result: std::io::Result<()>| result.unwrap_err().kind();
        assert_eq!(
            invalid(
                retain_video_artifact_path(
                    again,
                    fresh.clone(),
                    &other_recording,
                    &crate::control::ArtifactBudgets::shipping(),
                )
                .map(drop)
            ),
            std::io::ErrorKind::InvalidInput
        );
        assert_eq!(
            invalid(first.arm_video_retention_sweep(&other_recording)),
            std::io::ErrorKind::InvalidInput
        );
        // A recording replaced at its name: neither the new directory there nor
        // the old handle, its name now elsewhere, can arm the lease.
        let aside = root_path.join("rec-aside");
        std::fs::rename(root_path.join(&fresh), &aside).unwrap();
        std::fs::create_dir(root_path.join(&fresh)).unwrap();
        let impostor = crate::pinned_dir::PinnedDir::open_resolved(&root_path)
            .unwrap()
            .child(&fresh)
            .unwrap();
        for handle in [&impostor, &first_recording] {
            assert_eq!(
                invalid(first.arm_video_retention_sweep(handle)),
                std::io::ErrorKind::NotFound
            );
        }
        drop(impostor);
        std::fs::remove_dir(root_path.join(&fresh)).unwrap();
        std::fs::rename(&aside, root_path.join(&fresh)).unwrap();
        drop((first_recording, other_recording));

        let original_root = dir.join("recordings-original");
        std::fs::rename(&root_path, &original_root).unwrap();
        ensure_private_dir(&root_path).unwrap();
        std::fs::rename(original_root.join(&fresh), root_path.join(&fresh)).unwrap();

        let replacement_root = crate::pinned_dir::PinnedDir::open_resolved(&root_path).unwrap();
        let same_recording = replacement_root.child(&fresh).unwrap();
        let error = retain_video_artifact_path(
            replacement_root,
            fresh,
            &same_recording,
            &crate::control::ArtifactBudgets::shipping(),
        )
        .expect_err("the same recording inode cannot cross into a replacement root");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(artifact_lease_count(first.key.as_ref().unwrap()), Some(1));
        // Nor can it arm the original lease, whose root is no longer at the path.
        assert_eq!(
            invalid(first.arm_video_retention_sweep(&same_recording)),
            std::io::ErrorKind::NotFound
        );

        drop(same_recording);
        drop(first);
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
        // An in-flight recording (older stamp, no index.json yet) survives the prune.
        let in_flight = root.join("rec-00000000000000000000-999");
        std::fs::create_dir(&in_flight).unwrap();

        // Mint and abort perform no retention: a refused or failed request must not delete
        // a prior good recording.
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

        // Every probe paid for and the rename one unit short: nothing is renamed.
        let limit = 10 * VIDEO_SCAN_WORK + 20 * VIDEO_FILE_OPEN_WORK + (VIDEO_DELETE_WORK - 1);
        let mut work = VideoPruneWork::new(limit);
        prune_video_dirs_with_work(&pinned, &fresh_name, &mut work).unwrap();
        assert_eq!((work.file_opens, work.renames), (20, 0));
        assert!(root.join(format!("rec-{:020}-000", 0)).is_dir());
        // A budget that covers a step exactly pays for it.
        let mut exact = VideoPruneWork::new(VIDEO_SCAN_WORK);
        assert!(exact.scan() && !exact.scan());

        // A tombstone that cannot be emptied pays for the entry it tried, never
        // for the rmdir that cannot follow.
        #[cfg(target_os = "macos")]
        {
            let stuck = std::ffi::OsString::from(".prune-stuck");
            std::fs::create_dir(root.join(&stuck)).unwrap();
            std::fs::write(root.join(&stuck).join("frame"), b"png").unwrap();
            let undeletable = Immutable::set(&root.join(&stuck).join("frame"));
            let tombstone = pinned.child(&stuck).unwrap();
            let mut work = VideoPruneWork::new(VIDEO_PRUNE_WORK_LIMIT);
            let cleanup = cleanup_video_tombstone(&pinned, &stuck, &tombstone, &mut work);
            assert!(!cleanup.removed);
            assert_eq!(work.deletes, 1);
            drop((undeletable, tombstone));
            std::fs::remove_dir_all(root.join(&stuck)).unwrap();
        }

        // A recording a reader holds keeps its place and is no candidate: the
        // two oldest unheld ones are the only recordings renamed.
        let held = std::ffi::OsString::from(format!("rec-{:020}-000", 0));
        let lease_root = crate::pinned_dir::PinnedDir::open_resolved(&root).unwrap();
        let held_recording = lease_root.child(&held).unwrap();
        let lease = retain_video_artifact_path(
            lease_root,
            held.clone(),
            &held_recording,
            &crate::control::ArtifactBudgets::shipping(),
        )
        .unwrap()
        .expect("reader lease");
        let mut work = VideoPruneWork::new(VIDEO_PRUNE_WORK_LIMIT);
        prune_video_dirs_with_work(&pinned, &fresh_name, &mut work).unwrap();
        drop((lease, held_recording));
        assert_eq!(work.renames, 2);
        assert!(root.join(&held).is_dir());
        for taken in 1..=2 {
            assert!(!root.join(format!("rec-{taken:020}-000")).exists());
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Automatic-image namespace GC owns another retained directory chain. It
    /// must not start until the target, sealed file, and reply lease release
    /// theirs, especially when only one file-open worth of headroom remains.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn automatic_image_gc_waits_for_final_capability_under_low_nofile() {
        if !enter_low_nofile_test_child(16) {
            return;
        }

        // Shallow: confinement walks the root a component at a time under the limit.
        let temp = crate::test_scratch::short_dir("the automatic image root");
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
        if !enter_low_nofile_test_child(16) {
            return;
        }

        // Shallow: confinement walks the root a component at a time under the limit.
        let temp = crate::test_scratch::short_dir("the video root");
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

        // Neither a completed-looking directory that is no recording, nor a
        // recording with a marker but no index, is a candidate or takes a place;
        // one whose marker has a second name is never pruned on its word.
        write_completed_video_fixture(root, "operator-kept");
        let marker_only = root.join("rec-00000000000000000000-001");
        std::fs::create_dir(&marker_only).unwrap();
        std::fs::write(marker_only.join(VIDEO_PUBLISHED_FILE), b"published").unwrap();
        let linked_marker = root.join("rec-00000000000000000000-002");
        std::fs::create_dir(&linked_marker).unwrap();
        std::fs::write(linked_marker.join("index.json"), b"{\"frames\":[]}").unwrap();
        std::fs::write(root.join("marker-twin"), b"twin").unwrap();
        std::fs::hard_link(
            root.join("marker-twin"),
            linked_marker.join(VIDEO_PUBLISHED_FILE),
        )
        .unwrap();
        // A reader's lease on a recording of the same name under another root
        // protects nothing here.
        let elsewhere = aterm_tempfile::tempdir().expect("temporary second root");
        let same_name = std::ffi::OsString::from("rec-00000000000000000002-000");
        write_completed_video_fixture(elsewhere.path(), same_name.to_str().unwrap());
        let elsewhere_root = crate::pinned_dir::PinnedDir::open_resolved(elsewhere.path()).unwrap();
        let elsewhere_recording = elsewhere_root.child(&same_name).unwrap();
        let elsewhere_lease = retain_video_artifact_path(
            elsewhere_root,
            same_name.clone(),
            &elsewhere_recording,
            &crate::control::ArtifactBudgets::shipping(),
        )
        .unwrap()
        .expect("reader lease elsewhere");
        // A quarantine a crashed sweep left half done is finished first.
        let crashed = root.join(".prune-crashed");
        std::fs::create_dir(&crashed).unwrap();
        std::fs::write(crashed.join("frame-000000.png"), b"png").unwrap();
        // One the sweep cannot quarantine is no removal: the next oldest goes.
        #[cfg(target_os = "macos")]
        let unrenamable = root.join("rec-00000000000000000003-000");
        #[cfg(target_os = "macos")]
        let _unrenamable = Immutable::set(&unrenamable);

        let fresh = std::ffi::OsString::from("rec-99999999999999999999-000");
        write_completed_video_fixture(root, fresh.to_str().unwrap());
        let pinned = crate::pinned_dir::PinnedDir::open_resolved(root).unwrap();
        prune_video_dirs(&pinned, &fresh).unwrap();
        drop((elsewhere_lease, elsewhere_recording));
        assert!(root.join("operator-kept").is_dir() && marker_only.is_dir());
        assert!(linked_marker.is_dir() && !root.join(&same_name).exists());
        assert!(!crashed.exists());
        #[cfg(target_os = "macos")]
        assert!(unrenamable.is_dir() && !root.join("rec-00000000000000000004-000").exists());

        assert!(
            !corrupt.exists(),
            "retention completion is the marker/index identity pair, not JSON parsing"
        );
        assert!(
            !oversized.exists(),
            "retention never reads an attacker-sized completed index"
        );
        assert_eq!(completed_video_fixture_count(root), VIDEO_KEEP + 1);
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

        // Retention refused at the marker (its namespace moved aside for that
        // instant) fails the publication, though the marker stays visible.
        #[cfg(unix)]
        {
            let mut raced = confine_video_dir_for_instance(temp.path(), &instance).unwrap();
            let raced_path = raced.path().to_path_buf();
            let index = raced
                .write_batch_member_authorized(
                    std::ffi::OsStr::new("index.json"),
                    b"{\"frames\":[]}",
                    || true,
                )
                .unwrap();
            raced.publish(&[], &index).unwrap();
            let namespace = raced_path.parent().unwrap().to_path_buf();
            let aside = namespace.with_extension("aside");
            race_at(RaceWindow::MarkerVisible, {
                let (namespace, aside) = (namespace.clone(), aside.clone());
                move || std::fs::rename(&namespace, &aside).unwrap()
            });
            let published = raced.publish_marker_with_test_hook(|| {
                std::fs::rename(&aside, &namespace).unwrap();
            });
            assert!(race_ran());
            assert!(published.is_err());
            assert!(raced_path.join(VIDEO_PUBLISHED_FILE).is_file());
        }
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

    /// A child name is judged raw: `"x/"` cannot reach through the dangling link `x` and make
    /// (and chmod) a directory outside `parent`.
    #[test]
    #[cfg(unix)]
    fn a_direct_child_name_with_a_trailing_slash_makes_nothing_outside() {
        use std::os::unix::fs::symlink;
        let root =
            std::env::temp_dir().join(format!("aterm-direct-child-slash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let parent = root.join("parent");
        let outside = root.join("outside");
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(outside.join("absent"), parent.join("dangle")).unwrap();

        for name in ["dangle/", "dangle/.", "dangle//"] {
            let result = ensure_canonical_direct_child(&parent, name);
            assert!(result.is_err(), "{name:?} was accepted: {result:?}");
            assert!(
                !outside.join("absent").exists(),
                "{name:?} made a directory outside its parent"
            );
        }

        // Something already at the name must be a real directory: a link to a
        // sibling, or a file, is refused before anything is hardened through it.
        use std::os::unix::fs::PermissionsExt as _;
        let sibling = parent.join("sibling");
        std::fs::create_dir(&sibling).unwrap();
        std::fs::set_permissions(&sibling, std::fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&sibling, parent.join("linked")).unwrap();
        std::fs::write(parent.join("file"), b"").unwrap();
        for name in ["linked", "file"] {
            assert_eq!(
                ensure_canonical_direct_child(&parent, name)
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::PermissionDenied,
                "{name}"
            );
        }
        let mode = |dir: &Path| std::fs::metadata(dir).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode(&sibling), 0o755);
        // A real directory already there is hardened before it is used.
        assert_eq!(
            ensure_canonical_direct_child(&parent, "sibling").unwrap(),
            std::fs::canonicalize(&sibling).unwrap()
        );
        assert_eq!(mode(&sibling), 0o700);

        // A swap that lands after the look. A real directory when looked at and
        // a link to a stranger when resolved is refused before anything is
        // hardened through the link.
        let stranger = root.join("stranger");
        std::fs::create_dir(&stranger).unwrap();
        std::fs::set_permissions(&stranger, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir(parent.join("raced")).unwrap();
        race_at(RaceWindow::ChildLooked, {
            let (raced, stranger) = (parent.join("raced"), stranger.clone());
            move || {
                std::fs::remove_dir(&raced).unwrap();
                symlink(&stranger, &raced).unwrap();
            }
        });
        let raced = ensure_canonical_direct_child(&parent, "raced");
        assert!(race_ran());
        assert_eq!(
            raced.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(mode(&stranger), 0o755);
        // Nothing there when looked at, then a link to the parent's own
        // `sibling` planted before the create: its target is a direct child,
        // but the link is not one, and it is never handed back.
        race_at(RaceWindow::ChildLooked, {
            let (planted, sibling) = (parent.join("planted"), sibling.clone());
            move || symlink(&sibling, &planted).unwrap()
        });
        let planted = ensure_canonical_direct_child(&parent, "planted");
        assert!(race_ran());
        assert_eq!(
            planted.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        // The parent itself moved behind a link after the look: the real
        // directory made through it is no child of the parent that was resolved.
        let behind = root.join("parent.behind");
        race_at(RaceWindow::ChildLooked, {
            let (parent, behind) = (parent.clone(), behind.clone());
            move || {
                std::fs::rename(&parent, &behind).unwrap();
                symlink(&behind, &parent).unwrap();
            }
        });
        let moved = ensure_canonical_direct_child(&parent, "moved");
        assert!(race_ran());
        assert_eq!(
            moved.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        let _ = std::fs::remove_dir_all(&root);
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
        let lease =
            HeldAdvisoryLock::try_lock(open_instance_lease(&reused, true).unwrap()).unwrap();
        // Released by `LOCK_UN` (the guard's drop), not the close alone: a concurrent test's
        // fork between open and drop would keep the lock alive in its copy of the open file
        // description until it execs, and the sweep below would read the lease as Held.
        drop(lease);
        sweep_dead_instance_namespaces_with(&root, "p1-current", |_| true);
        assert!(
            !reused.exists(),
            "an unlocked exact-instance lease wins over a reused live PID"
        );

        // PID false-negative case: a held exact-instance lease is authoritative
        // and survives even when the compatibility liveness probe says dead.
        let held = ensure_canonical_direct_child(&root, "p424243-held").unwrap();
        let lease = HeldAdvisoryLock::try_lock(open_instance_lease(&held, true).unwrap()).unwrap();
        sweep_dead_instance_namespaces_with(&root, "p1-current", |_| false);
        assert!(
            held.is_dir(),
            "a contended exact-instance lease is never swept"
        );
        drop(lease);

        // Never the sweeper's own namespace, nor an entry that names no
        // instance (`p<pid>-<nonce>`), whatever the PID probe says.
        let current = ensure_canonical_direct_child(&root, "p424240-current").unwrap();
        let unnamed = ["not-an-instance", "424241-unprefixed", "p424242"]
            .map(|name| ensure_canonical_direct_child(&root, name).unwrap());
        // A tombstone an interrupted sweep left is finished by the next.
        let tombstone = root.join(format!("{DEAD_INSTANCE_TOMBSTONE_PREFIX}left"));
        std::fs::create_dir(&tombstone).unwrap();
        std::fs::write(tombstone.join(INSTANCE_LEASE_FILE), b"").unwrap();
        sweep_dead_instance_namespaces_with(&root, "p424240-current", |_| false);
        assert!(current.is_dir() && unnamed.iter().all(|dir| dir.is_dir()));
        assert!(!tombstone.exists());

        // With no delete left, an emptied tombstone stays for a later pass.
        std::fs::create_dir(&tombstone).unwrap();
        let pinned_root = crate::pinned_dir::PinnedDir::open_resolved(&root).unwrap();
        let name = tombstone.file_name().unwrap();
        let pinned_tombstone = pinned_root.child(name).unwrap();
        let mut budget = 0;
        assert!(!cleanup_instance_tombstone(
            &pinned_root,
            name,
            &pinned_tombstone,
            &mut budget
        ));
        assert!(tombstone.is_dir());
        // One delete left removes it, and is spent.
        let mut budget = 1;
        assert!(cleanup_instance_tombstone(
            &pinned_root,
            name,
            &pinned_tombstone,
            &mut budget
        ));
        assert!(!tombstone.exists() && budget == 0);
        drop((pinned_tombstone, pinned_root));

        // A namespace this process confined into is leased for its lifetime,
        // so a sweep that reads its PID as dead still keeps it.
        let ours = format!("p{}-leased", std::process::id());
        drop(confine_video_dir_for_instance(&root, &ours).unwrap());
        sweep_dead_instance_namespaces_with(&root.join(VIDEO_DIR), "p1-current", |_| false);
        assert!(root.join(VIDEO_DIR).join(&ours).is_dir());
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
        let held_lease =
            HeldAdvisoryLock::try_lock(open_instance_lease(&original_namespace, true).unwrap())
                .unwrap();

        // The replacement advertises the same namespace name but an unlocked
        // lease. A lexical lease reopen after the swap would misclassify the
        // live original as abandoned and delete it through the retained root.
        let replacement_namespace = ensure_canonical_direct_child(&replacement, name).unwrap();
        drop(open_instance_lease(&replacement_namespace, true).unwrap());

        let budgets = crate::control::ArtifactBudgets::shipping();
        let queue = ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue");
        let admission = budgets
            .try_reserve_cleanup_for_path(&root)
            .expect("test cleanup admission");
        let mut sweep = DeadNamespaceSweep::begin(&root, "p1-current".into(), queue, admission)
            .expect("pinned sweep");
        let mut swapped = false;
        assert!(sweep.run_batch_with_hook(
            DEAD_INSTANCE_DELETE_LIMIT,
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
        drop(held_lease);

        // The same swap under a launch that is gone: the namespace the sweep
        // proved abandoned is left alone once its root has moved away.
        let begin = |root: &Path| {
            let budgets = crate::control::ArtifactBudgets::shipping();
            let admission = budgets
                .try_reserve_cleanup_for_path(root)
                .expect("test cleanup admission");
            let queue =
                ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue");
            DeadNamespaceSweep::begin(root, "p1-current".into(), queue, admission)
                .expect("pinned sweep")
        };
        let mut sweep = begin(&root);
        let mut swapped = false;
        assert!(sweep.run_batch_with_hook(
            DEAD_INSTANCE_DELETE_LIMIT,
            |_| false,
            || {
                if !swapped {
                    std::fs::rename(&root, &moved).unwrap();
                    std::fs::rename(&replacement, &root).unwrap();
                    swapped = true;
                }
            }
        ));
        assert!(swapped && moved.join(name).is_dir());
        drop(sweep);
        std::fs::rename(&root, &replacement).unwrap();
        std::fs::rename(&moved, &root).unwrap();

        // A root moved away before a batch ends the cursor: not even a
        // tombstone in it is the sweep's any more.
        let tombstone = format!("{DEAD_INSTANCE_TOMBSTONE_PREFIX}left");
        std::fs::create_dir(root.join(&tombstone)).unwrap();
        let mut sweep = begin(&root);
        std::fs::rename(&root, &moved).unwrap();
        assert!(sweep.run_batch(|_| false));
        assert!(moved.join(&tombstone).is_dir());
        drop(sweep);

        // A root moved between its pin and its listing is not begun at all: the
        // cursor would list the replacement while it acts on the pinned root.
        std::fs::rename(&moved, &root).unwrap();
        race_at(RaceWindow::CursorPinned, {
            let (root, moved, replacement) = (root.clone(), moved.clone(), replacement.clone());
            move || {
                std::fs::rename(&root, &moved).unwrap();
                std::fs::rename(&replacement, &root).unwrap();
            }
        });
        let budgets = crate::control::ArtifactBudgets::shipping();
        let admission = budgets
            .try_reserve_cleanup_for_path(&root)
            .expect("test cleanup admission");
        let queue = ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue");
        let raced = DeadNamespaceSweep::begin(&root, "p1-current".into(), queue, admission);
        assert!(race_ran());
        assert!(raced.is_err());
        drop(raced);

        // The delete budget is a hard stop: one delete allowed, one of two dead
        // namespaces is taken and the other waits for a later batch.
        let budgeted = temp.path().join("budgeted");
        ensure_private_dir(&budgeted).unwrap();
        let dead = ["p424250-dead", "p424251-dead"];
        for name in dead {
            let namespace = ensure_canonical_direct_child(&budgeted, name).unwrap();
            drop(open_instance_lease(&namespace, true).unwrap());
        }
        let mut sweep = begin(&budgeted);
        assert!(!sweep.run_batch_with_hook(1, |_| false, || {}));
        let left = dead.iter().filter(|name| budgeted.join(name).is_dir());
        assert_eq!(left.count(), 1);
        // The rename that took it was that one delete: nothing was emptied.
        for entry in std::fs::read_dir(&budgeted).unwrap().flatten() {
            assert!(
                entry.path().join(INSTANCE_LEASE_FILE).is_file(),
                "{entry:?}"
            );
        }
    }

    /// The sweep lets go of a dead instance's lease by `LOCK_UN` on every way out, here the
    /// `continue` after the identity re-check that follows the take: a lease released by the
    /// close alone stays locked in a child forked under the sweep until it execs, and the next
    /// sweep reads the dead instance as live.
    ///
    /// The fork is real, made inside the sweep's hold of the lease ([`crate::parked_fork`]),
    /// and parked short of its exec while the lease is probed and the next sweep runs. The
    /// hook moves the namespace out of the root after the sweep pinned it, so the sweep takes
    /// the lease through its retained handle and then leaves by that `continue`.
    #[cfg(unix)]
    #[test]
    fn a_dead_instances_lease_is_free_while_a_child_forked_under_the_sweep_has_yet_to_exec() {
        let temp = aterm_tempfile::tempdir().expect("temporary namespace root");
        let root = temp.path().join("root");
        let aside = temp.path().join("aside");
        ensure_private_dir(&root).unwrap();
        ensure_private_dir(&aside).unwrap();
        let name = "p424245-dead";
        let namespace = ensure_canonical_direct_child(&root, name).unwrap();
        // A launch that is gone: its lease exists and nobody holds it.
        drop(open_instance_lease(&namespace, true).unwrap());
        let moved = aside.join(name);

        let budgets = crate::control::ArtifactBudgets::shipping();
        let queue = ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue");
        let admission = budgets
            .try_reserve_cleanup_for_path(&root)
            .expect("test cleanup admission");
        let mut sweep = DeadNamespaceSweep::begin(&root, "p1-current".into(), queue, admission)
            .expect("pinned sweep");
        let armed = crate::parked_fork::arm_at(1); // the sweep's take of the lease
        let mut moved_aside = false;
        assert!(sweep.run_batch_with_hook(
            DEAD_INSTANCE_DELETE_LIMIT,
            |_| true,
            || {
                if !moved_aside {
                    std::fs::rename(&namespace, &moved).unwrap();
                    moved_aside = true;
                }
            }
        ));
        drop(sweep);
        assert!(moved_aside, "the hook ran before the lease was taken");
        let child = armed.child();
        assert!(child.still_parked(), "forked under the sweep's lease");
        assert!(
            moved.is_dir(),
            "the identity re-check failed, so the sweep left the namespace alone"
        );

        let probe = HeldAdvisoryLock::try_lock(open_instance_lease(&moved, false).unwrap());
        assert!(
            probe.is_ok(),
            "the sweep's lease stayed locked in the child forked under it: the sweep \
             let it go by the close alone"
        );
        drop(probe);

        // What that residue cost: the next sweep, while the child is still short
        // of its exec, must read the gone launch as gone.
        std::fs::rename(&moved, root.join(name)).unwrap();
        sweep_dead_instance_namespaces_with(&root, "p1-current", |_| true);
        assert!(
            !root.join(name).exists(),
            "a dead instance's namespace is swept while the forked child has yet to exec"
        );
        assert!(child.still_parked(), "…while the child had yet to exec");
        child.release();
    }

    #[test]
    fn dead_namespace_debt_after_cursor_open_restarts_with_fresh_generation() {
        let temp = aterm_tempfile::tempdir().expect("temporary namespace root");
        let root = std::fs::canonicalize(temp.path()).unwrap();
        pending_dead_namespace_sweeps()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(root.clone(), PendingDeadNamespaceSweep { dirty: false });

        let budgets = crate::control::ArtifactBudgets::shipping();
        let admission = budgets
            .try_reserve_cleanup_for_path(&root)
            .expect("test cleanup admission");
        let mut sweep = DeadNamespaceSweep::begin(
            &root,
            "p1-current".into(),
            ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue"),
            admission,
        )
        .expect("initial cursor");

        // Model another lifecycle release after the cursor snapshot opened.
        // ReadDir need not reveal any corresponding entry to this cursor; the
        // dirty generation itself must force a new one after EOF.
        schedule_dead_instance_namespace_sweep(
            &root,
            "p1-current",
            ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue"),
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
        // The restart took the debt: its generation, finished with no new
        // kick, retires the root instead of restarting again.
        finish_or_restart_dead_namespace_sweep(restarted_root, current, _queue, &sender);
        assert!(receiver.try_recv().is_err());
        assert!(
            !pending_dead_namespace_sweeps()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(&root)
        );

        // A cursor stopped by its scan slice is requeued to go on: clutter
        // ahead of a stale namespace never hides it.
        let wide = root.join("wide");
        std::fs::create_dir(&wide).unwrap();
        for entry in 0..=DEAD_INSTANCE_SCAN_LIMIT {
            std::fs::write(wide.join(format!("clutter-{entry}")), b"").unwrap();
        }
        let task = ArtifactCleanupTask::DeadNamespaces {
            root: wide,
            current: "p1-current".into(),
            _queue: ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue"),
        };
        run_best_effort_cleanup_task(task, &sender, &budgets);
        assert!(matches!(
            receiver.try_recv(),
            Ok(ArtifactCleanupTask::DeadNamespaceContinuation(_))
        ));
    }

    /// Run the shipped worker loop, `artifact_cleanup_worker`, over a private queue seeded
    /// with `tasks` and an already-closed wake channel, and return once that loop does.
    ///
    /// With its wake channel closed, the loop returns the first time both queues are empty,
    /// and only the worker can enqueue after seeding, so its return means every seeded task
    /// and every recovery, continuation, restart or requeue it produced has completed. An
    /// artifact vanishing from disk is not that event: a sweep renames its target away well
    /// before it syncs the root and reconciles its coalescing entry. The shared worker offers
    /// no such event, since every other test feeds it. The timeout only turns a wedged worker
    /// into a failure instead of a hang.
    fn drain_private_cleanup_worker(tasks: Vec<ArtifactCleanupTask>) {
        let (video_sender, video_receiver) = std::sync::mpsc::channel::<VideoRetentionTask>();
        let (best_effort, best_effort_receiver) = std::sync::mpsc::channel();
        let (wake, wake_receiver) = std::sync::mpsc::sync_channel::<()>(1);
        drop(video_sender);
        drop(wake);
        for task in tasks {
            best_effort
                .send(task)
                .expect("the private cleanup queue is open");
        }
        let (returned, worker_returned) = std::sync::mpsc::sync_channel(1);
        // No `aterm-` prefix: that names a shipped worker, which the census in qos.rs holds
        // to a scheduling role.
        let worker = std::thread::Builder::new()
            .name("private-artifact-cleanup-worker".into())
            .spawn(move || {
                artifact_cleanup_worker(
                    &video_receiver,
                    &best_effort_receiver,
                    &wake_receiver,
                    &best_effort,
                    &crate::control::ArtifactBudgets::shipping(),
                );
                let _ = returned.send(());
            })
            .expect("spawn the private cleanup worker");
        match worker_returned.recv_timeout(std::time::Duration::from_secs(60)) {
            Ok(()) => worker.join().expect("the private cleanup worker returned"),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // The worker thread died outside its own catch_unwind.
                std::panic::resume_unwind(worker.join().expect_err("the worker unwound"));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("the private cleanup worker never drained its queue after 60 s")
            }
        }
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

        // Both debts are outstanding at once and one worker loop takes both
        // injected panics back to back, so the drain also proves the loop
        // survives the first panic to recover the second debt. The dead-
        // namespace sweep skips `video-tombstones`: it is neither a PID-shaped
        // instance name nor an instance tombstone.
        let budgets = crate::control::ArtifactBudgets::shipping();
        drain_private_cleanup_worker(vec![
            ArtifactCleanupTask::PanicForTest(ArtifactCleanupPanicRecovery::DeadNamespaces {
                root: root.clone(),
                current: "p1-current".into(),
                queue: ArtifactCleanupQueuePermit::try_reserve(&budgets)
                    .expect("test cleanup queue"),
            }),
            ArtifactCleanupTask::PanicForTest(ArtifactCleanupPanicRecovery::VideoTombstones {
                root: video_root.clone(),
                expected_root,
                queue: ArtifactCleanupQueuePermit::try_reserve(&budgets)
                    .expect("test cleanup queue"),
            }),
        ]);

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

    /// A video producer's deferred cleanup asks for a tombstone sweep only when
    /// its abort left a tombstone; a published recording's asks for none. The
    /// dead-namespace sweep is asked for either way.
    #[test]
    fn only_an_armed_video_cleanup_asks_for_a_tombstone_sweep() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let identity = crate::pinned_dir::PinnedDir::open(&root)
            .unwrap()
            .retained_identity()
            .unwrap();
        let dead_root = root.join("dead");
        // Pending entries this test owns coalesce the drop's requests, so each
        // shows as `dirty` and no worker runs.
        pending_video_tombstone_sweeps()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(PendingVideoTombstoneSweep {
                root: root.clone(),
                expected_root: identity,
                dirty: false,
            });
        pending_dead_namespace_sweeps()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                dead_root.clone(),
                PendingDeadNamespaceSweep { dirty: false },
            );
        let requested = || {
            let video = pending_video_tombstone_sweeps()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .any(|entry| entry.root == root && entry.dirty);
            let dead = pending_dead_namespace_sweeps()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&dead_root)
                .is_some_and(|entry| entry.dirty);
            (video, dead)
        };
        let budgets = crate::control::ArtifactBudgets::shipping();
        let queue = || ArtifactCleanupQueuePermit::try_reserve(&budgets);
        let deferred = |armed: bool| {
            let mut cleanup = DeferredVideoCleanup {
                tombstone_root: root.clone(),
                tombstone_identity: identity,
                tombstone_queue: queue(),
                tombstone_armed: false,
                dead_root: dead_root.clone(),
                current: "p1-current".into(),
                dead_queue: queue(),
            };
            if armed {
                cleanup.arm_tombstone();
            }
            cleanup
        };

        drop(deferred(false));
        assert_eq!(requested(), (false, true));
        drop(deferred(true));
        assert_eq!(requested(), (true, true));
        clear_pending_video_tombstone_sweep(&root, identity);
        clear_pending_dead_namespace_sweep(&dead_root);
    }

    /// The automatic-image sweep prunes only the directory it was armed for:
    /// a replacement standing at the same path is not its to prune.
    #[test]
    fn the_automatic_image_sweep_prunes_only_the_directory_it_was_armed_for() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let auto = std::fs::canonicalize(temp.path())
            .unwrap()
            .join(AUTO_IMAGES_DIR);
        let dir = auto.join(process_instance_id());
        std::fs::create_dir_all(&dir).unwrap();
        let identity = |dir: &Path| {
            crate::pinned_dir::PinnedDir::open(dir)
                .unwrap()
                .retained_identity()
                .unwrap()
        };
        let armed_for = identity(&dir);
        std::fs::rename(&dir, auto.join("moved")).unwrap();
        std::fs::create_dir(&dir).unwrap();
        let name = |sequence| automatic_capture_name_for("image", process_instance_id(), sequence);
        for sequence in 0..=(AUTO_IMAGE_KEEP as u64 + 2) {
            std::fs::write(dir.join(name(sequence)), b"png").unwrap();
        }
        let fresh = std::ffi::OsString::from(name(AUTO_IMAGE_KEEP as u64 + 2));
        // A directory named like a capture is none: it is not pruned, and it
        // costs no capture its place.
        let lookalike = dir.join(name(AUTO_IMAGE_KEEP as u64 + 3));
        std::fs::create_dir(&lookalike).unwrap();
        // Nor is the temporary a capture is still being written under (the
        // oldest sequence, were its number read as one).
        let in_flight = dir.join(format!(".aterm-write-p1-{:020}", 0));
        std::fs::write(&in_flight, b"partial").unwrap();
        let budgets = crate::control::ArtifactBudgets::shipping();
        let sweep = |expected_dir| ArtifactCleanupTask::AutomaticImages {
            dir: dir.clone(),
            expected_dir,
            fresh: fresh.clone(),
            _queue: ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue"),
        };
        let count = || {
            std::fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
                .count()
        };

        drain_private_cleanup_worker(vec![sweep(armed_for)]);
        assert_eq!(
            count(),
            AUTO_IMAGE_KEEP + 3,
            "the replacement is left whole"
        );
        // Moved between the sweep's pin and its look at the path: the pin is
        // no longer what the path names, so nothing is pruned through it.
        #[cfg(unix)]
        {
            let raced = auto.join("raced");
            race_at(RaceWindow::ImagesPinned, {
                let (dir, raced) = (dir.clone(), raced.clone());
                move || {
                    std::fs::rename(&dir, &raced).unwrap();
                    std::fs::create_dir(&dir).unwrap();
                }
            });
            let (requeue, _requeued) = std::sync::mpsc::channel();
            run_best_effort_cleanup_task(sweep(identity(&dir)), &requeue, &budgets);
            assert!(race_ran());
            std::fs::remove_dir(&dir).unwrap();
            std::fs::rename(&raced, &dir).unwrap();
            assert_eq!(count(), AUTO_IMAGE_KEEP + 3);
        }
        drain_private_cleanup_worker(vec![sweep(identity(&dir))]);
        assert_eq!(
            count(),
            AUTO_IMAGE_KEEP,
            "positive control: armed for it, it is pruned"
        );
        assert!(lookalike.is_dir() && in_flight.is_file());
    }

    /// The tombstone sweep removes `.prune-*` entries only, and only under the
    /// root it was armed for.
    #[test]
    fn the_video_tombstone_sweep_removes_only_tombstones_of_its_own_root() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap().join("instance");
        std::fs::create_dir(&root).unwrap();
        let identity = |dir: &Path| {
            crate::pinned_dir::PinnedDir::open(dir)
                .unwrap()
                .retained_identity()
                .unwrap()
        };
        let armed_for = identity(&root);
        std::fs::rename(&root, temp.path().join("moved")).unwrap();
        for entry in [".prune-x", "rec-kept"] {
            std::fs::create_dir_all(root.join(entry)).unwrap();
            std::fs::write(root.join(entry).join("index.json"), b"{}").unwrap();
        }
        let budgets = crate::control::ArtifactBudgets::shipping();
        let sweep = |expected_root| ArtifactCleanupTask::VideoTombstones {
            root: root.clone(),
            expected_root,
            _queue: ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue"),
        };

        drain_private_cleanup_worker(vec![sweep(armed_for)]);
        assert!(
            root.join(".prune-x").is_dir(),
            "a replacement root is not its to sweep"
        );
        drain_private_cleanup_worker(vec![sweep(identity(&root))]);
        assert!(
            !root.join(".prune-x").exists(),
            "positive control: its own tombstone goes"
        );
        assert!(
            root.join("rec-kept").is_dir(),
            "a recording is no tombstone"
        );

        let begin = |root: &Path| {
            let budgets = crate::control::ArtifactBudgets::shipping();
            let admission = budgets
                .try_reserve_cleanup_for_path(root)
                .expect("test cleanup admission");
            let queue =
                ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue");
            VideoTombstoneSweep::begin(root, identity(root), queue, admission).expect("cursor")
        };
        // A root moved away from under a begun cursor ends it (Windows will not
        // rename a directory the cursor holds open).
        #[cfg(unix)]
        {
            std::fs::create_dir(root.join(".prune-y")).unwrap();
            let mut cursor = begin(&root);
            let aside = root.with_file_name("aside");
            std::fs::rename(&root, &aside).unwrap();
            assert!(cursor.run_batch());
            assert!(aside.join(".prune-y").is_dir());
            drop(cursor);

            // Moved between its pin and its listing, it is not begun at all:
            // the cursor would list the replacement while acting on the pin.
            std::fs::rename(&aside, &root).unwrap();
            let expected_root = identity(&root);
            race_at(RaceWindow::CursorPinned, {
                let (root, aside) = (root.clone(), aside.clone());
                move || {
                    std::fs::rename(&root, &aside).unwrap();
                    std::fs::create_dir(&root).unwrap();
                }
            });
            let budgets = crate::control::ArtifactBudgets::shipping();
            let admission = budgets
                .try_reserve_cleanup_for_path(&root)
                .expect("test cleanup admission");
            let queue =
                ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue");
            let raced = VideoTombstoneSweep::begin(&root, expected_root, queue, admission);
            assert!(race_ran());
            assert!(raced.is_none());
        }

        // A budget spent inside a tombstone stops the cursor short of EOF and
        // owes a restart, which finishing it then schedules.
        let partial = root.with_file_name("partial");
        std::fs::create_dir_all(partial.join(".prune-a")).unwrap();
        for file in ["one", "two"] {
            std::fs::write(partial.join(".prune-a").join(file), b"").unwrap();
        }
        let mut cursor = begin(&partial);
        assert!(!cursor.run_batch_within(VIDEO_SCAN_WORK + VIDEO_DELETE_WORK));
        assert!(cursor.restart_required);
        let expected_root = cursor.expected_root;
        pending_video_tombstone_sweeps()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(PendingVideoTombstoneSweep {
                root: partial.clone(),
                expected_root,
                dirty: false,
            });
        let (sender, receiver) = std::sync::mpsc::channel();
        finish_or_restart_video_tombstone_sweep(cursor, &sender);
        clear_pending_video_tombstone_sweep(&partial, expected_root);
        assert!(matches!(
            receiver.try_recv(),
            Ok(ArtifactCleanupTask::VideoTombstones { .. })
        ));
        // So does a budget too short to scan the next entry.
        let short = root.with_file_name("short");
        for tombstone in [".prune-b", ".prune-c"] {
            std::fs::create_dir_all(short.join(tombstone)).unwrap();
        }
        let mut cursor = begin(&short);
        assert!(!cursor.run_batch_within(VIDEO_SCAN_WORK * 3 / 2 + VIDEO_DELETE_WORK));
        assert!(cursor.restart_required);
        drop(cursor);

        // A sweep asked for while one is pending folds into it: the pending one
        // is marked to run again, and nothing more is queued.
        let short_identity = identity(&short);
        let pending = || {
            pending_video_tombstone_sweeps()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        };
        pending().push(PendingVideoTombstoneSweep {
            root: short.clone(),
            expected_root: short_identity,
            dirty: false,
        });
        schedule_video_tombstone_sweep(
            &short,
            short_identity,
            ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue"),
        );
        let dirty: Vec<bool> = pending()
            .iter()
            .filter(|entry| entry.root == short)
            .map(|entry| entry.dirty)
            .collect();
        assert_eq!(dirty, [true]);
        // The folded kick restarts the cursor once; that generation, finished
        // with no new kick, retires the root instead of restarting again.
        let (sender, receiver) = std::sync::mpsc::channel();
        for restarts in [true, false] {
            let mut cursor = begin(&short);
            assert!(cursor.run_batch());
            finish_or_restart_video_tombstone_sweep(cursor, &sender);
            assert_eq!(receiver.try_recv().is_ok(), restarts);
        }
        assert!(pending().iter().all(|entry| entry.root != short));

        // A cursor stopped by its scan slice is requeued to go on: clutter
        // ahead of a tombstone never hides it.
        let wide = root.with_file_name("wide");
        std::fs::create_dir(&wide).unwrap();
        for entry in 0..=VIDEO_PRUNE_SCAN_LIMIT {
            std::fs::write(wide.join(format!("clutter-{entry}")), b"").unwrap();
        }
        let task = ArtifactCleanupTask::VideoTombstones {
            expected_root: identity(&wide),
            root: wide,
            _queue: ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue"),
        };
        run_best_effort_cleanup_task(task, &sender, &budgets);
        assert!(matches!(
            receiver.try_recv(),
            Ok(ArtifactCleanupTask::VideoTombstoneContinuation(_))
        ));
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
            assert_eq!(std::fs::read(&victim).unwrap(), b"keep");

            // A lease with a second name is refused, and never chmodded through.
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode();
            let linked = ensure_canonical_direct_child(&root, "p424247-hardlease").unwrap();
            std::fs::hard_link(&victim, linked.join(INSTANCE_LEASE_FILE)).unwrap();
            let victim_mode = mode(&victim);
            assert_eq!(
                open_instance_lease(&linked, true).unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );
            assert_eq!(mode(&victim), victim_mode);

            // A lease left loose is tightened by the open that takes it.
            let loose = ensure_canonical_direct_child(&root, "p424248-looselease").unwrap();
            let loose_lease = loose.join(INSTANCE_LEASE_FILE);
            std::fs::write(&loose_lease, b"").unwrap();
            std::fs::set_permissions(&loose_lease, std::fs::Permissions::from_mode(0o644)).unwrap();
            drop(open_instance_lease(&loose, false).unwrap());
            assert_eq!(mode(&loose_lease) & 0o7777, 0o600);

            // A swap after the open: the lease renamed aside with another file
            // put in its place, or its namespace moved behind a link. The file
            // opened is no longer the lease its namespace names: refused.
            race_at(RaceWindow::LockOpened, {
                let lease = loose_lease.clone();
                move || {
                    std::fs::rename(&lease, lease.with_extension("aside")).unwrap();
                    std::fs::write(&lease, b"").unwrap();
                }
            });
            let swapped = open_instance_lease(&loose, false);
            assert!(race_ran());
            assert_eq!(
                swapped.unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );
            race_at(RaceWindow::LockOpened, {
                let namespace = loose.clone();
                move || {
                    let behind = namespace.with_extension("behind");
                    std::fs::rename(&namespace, &behind).unwrap();
                    symlink(&behind, &namespace).unwrap();
                }
            });
            let behind_link = open_instance_lease(&loose, false);
            assert!(race_ran());
            assert_eq!(
                behind_link.unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );

            // A lease that is a FIFO by the time it is opened, and gone once it
            // is: the object opened is refused, never reported as no lease.
            let piped = ensure_canonical_direct_child(&root, "p424249-fifolease").unwrap();
            let piped_lease = piped.join(INSTANCE_LEASE_FILE);
            std::fs::write(&piped_lease, b"").unwrap();
            race_at(RaceWindow::LockLooked, {
                let lease = piped_lease.clone();
                move || {
                    use std::os::unix::ffi::OsStrExt as _;
                    std::fs::remove_file(&lease).unwrap();
                    let fifo = std::ffi::CString::new(lease.as_os_str().as_bytes()).unwrap();
                    // SAFETY: a valid NUL-terminated path; mkfifo reads it and nothing else.
                    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
                    race_at(RaceWindow::LockOpened, move || {
                        std::fs::remove_file(&lease).unwrap();
                    });
                }
            });
            let piped_open = open_instance_lease(&piped, false);
            assert!(race_ran());
            assert_eq!(
                piped_open.unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );

            // A lease this open creates is 0600 from its first instant, even
            // when the open is then refused (a second name added after it).
            let refused = ensure_canonical_direct_child(&root, "p424250-refusedlease").unwrap();
            let refused_lease = refused.join(INSTANCE_LEASE_FILE);
            race_at(RaceWindow::LockOpened, {
                let (lease, second) = (refused_lease.clone(), refused.join("second"));
                move || std::fs::hard_link(&lease, &second).unwrap()
            });
            let refused_open = open_instance_lease(&refused, true);
            assert!(race_ran());
            assert_eq!(
                refused_open.unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );
            assert_eq!(mode(&refused_lease) & 0o7777, 0o600);
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
}

/// The artifact machines' environment actions and Buggy mutants, each waived where its
/// Tier-1 (`artifact_transaction_conformance`) binds it.
#[cfg(any(test, debug_assertions))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "SnapshotGenerationCommit",
    action = "SelectCurrent",
    reason = "environment scheduling: selecting the newest worker changes which queued job the \
              bounded model drives; begin/old-commit/current-commit are bound to shipping code"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AnchoredArtifactTransaction",
    action = "SwapAncestor",
    reason = "adversarial filesystem environment action: another same-uid process replaces a \
              pathname ancestor; the Unix Tier-1 test drives the observation explicitly"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AnchoredArtifactTransaction",
    action = "BuggyReresolveRead",
    reason = "Buggy=1 negative-control action only; shipping reads remain relative to the retained \
              directory handle and the Tier-1 outside-target mutant is rejected"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AnchoredArtifactTransaction",
    action = "BuggyReresolveWrite",
    reason = "Buggy=1 negative-control action only; shipping writes remain relative to the \
              retained directory handle and never implement this transition"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AnchoredArtifactTransaction",
    action = "BuggyCertifySwapped",
    reason = "Buggy=1 negative-control action only; reply validation fails closed after identity \
              drift and the Tier-1 false-success mutant is rejected"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyPublishAfterCancel",
    reason = "Buggy=1 negative control only; the real cancellation/authorization CAS has one \
              winner and the authorized write hook rejects a cancelled final-name publish"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyDropBeforeWrite",
    reason = "Buggy=1 negative control only; ControlReply owns ReplyRetention through write_all, \
              flush, and either valid ACK release or central-quarantine expiry"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyPruneLeased",
    reason = "Buggy=1 negative control only; retention holds the shared path-lease mutex across its \
              exact mutation and skips every still-leased image or recording"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyReleaseWithoutAck",
    reason = "Buggy=1 negative control only; only a matching nonce echo releases immediately, \
              while EOF/timeout/protocol failure transfers the guard to central quarantine"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyAcceptPreChallengeAck",
    reason = "Buggy=1 negative control only; aterm-ctl can echo only the fresh nonce trailer it \
              reads after the complete response, never a pre-pipelined acknowledgement"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyReleaseQuarantineEarly",
    reason = "Buggy=1 negative control only; failed or half-closed clients retain the publication guard \
              through the additional 30-second central-quarantine expiry"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyChallengeBeforeBody",
    reason = "Buggy=1 negative control only; ControlReply::into_wire queues the nonce trailer after \
              the complete body, and the Tier-1 early trailer is rejected"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyOkBeforeRevalidation",
    reason = "Buggy=1 negative control only; Outbox::attach revalidates the guard before any OK \
              byte is queued, and the Tier-1 OK-then-ERR wire is rejected"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyTrailerErrorIgnored",
    reason = "Buggy=1 negative control only; a failed trailer write returns the error and \
              quarantines the guard, and the Tier-1 partial frame read as written is rejected"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyAbortRetainsArtifact",
    reason = "Buggy=1 negative control only; an uncommitted reply guard removes its exact file on \
              drop, and the Tier-1 orphaned-file release is rejected"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactHandoffCapacity",
    action = "BuggyOverbook",
    reason = "Buggy=1 negative control only; the real locked admission update refuses when the \
              descriptor budget's path count or descriptor units reach its cap"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactHandoffCapacity",
    action = "BuggyReconcileOverbook",
    reason = "Buggy=1 negative control only; exact-path reconciliation uses the same locked \
              aggregate descriptor budget and leaves the provisional charge intact on refusal"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactHandoffCapacity",
    action = "BuggyRefuseLeaksSlot",
    reason = "Buggy=1 negative control only; UnitBudget::try_take decides admission before it \
              charges either counter, so a refusal leaves the pool untouched"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactHandoffCapacity",
    action = "BuggyReleaseProvisionalCharge",
    reason = "Buggy=1 negative control only; reconciliation records the permit's new charge, so \
              its drop returns exactly the units it holds"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactHandoffCapacity",
    action = "SelectOne",
    reason = "ghost scheduler choice selecting a one-unit permit from the bounded charge multiset"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactHandoffCapacity",
    action = "SelectTwo",
    reason = "ghost scheduler choice selecting a two-unit permit from the bounded charge multiset"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactHandoffCapacity",
    action = "SelectThree",
    reason = "ghost scheduler choice selecting a three-unit permit from the bounded charge multiset"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "VideoBatchPublicationDurability",
    action = "BuggyPublishBeforeSync",
    reason = "Buggy=1 negative control only; the real marker write is reachable only after \
              ConfinedVideoDir::publish completes the recording-directory batch barrier"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "VideoBatchPublicationDurability",
    action = "BuggySyncAhead",
    reason = "Buggy=1 negative control only; every member write clears batch_synced before it \
              lands, so a barrier never covers a later member \
              (video_member_write_after_sync_invalidates_marker_guard)"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactReaderLease",
    action = "BuggyStartSweepEarly",
    reason = "Buggy=1 negative control only; the concrete registry starts its capability-bound \
              sweep only on the last refcount release"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactReaderLease",
    action = "BuggyAcquireDuringSweep",
    reason = "Buggy=1 negative control only; retain_video_artifact_path fails closed while the \
              last-release callback owns the registry's sweeping state"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactReaderLease",
    action = "ReplaceIdentity",
    reason = "external same-uid recording replacement is an environment transition; the concrete \
              registry compares the retained child identity before incrementing its lease count"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactReaderLease",
    action = "BuggyArmedReleaseDropsEntry",
    reason = "Buggy=1 negative control only; the last release of an armed entry marks it \
              sweeping and hands its capability to the priority cleanup lane instead of \
              removing it"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactReaderLease",
    action = "BuggyReleaseSweepsUnarmed",
    reason = "Buggy=1 negative control only; the last release starts a sweep only when \
              video_sweep_requested is set, and otherwise removes the registry entry"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactReaderLease",
    action = "BuggyFinishKeepsEntry",
    reason = "Buggy=1 negative control only; finish_video_retention_sweep removes the finished \
              entry, taking its arm and recorded identity with it"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ArtifactReaderLease",
    action = "BuggyAcquireReplacedIdentity",
    reason = "Buggy=1 negative control only; a mismatched retained recording identity is rejected \
              without joining the existing lexical-path lease group"
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn artifact_environment_and_mutant_waivers() {}

/// `ExactInstanceRetention` environment actions, each waived where its Tier-1
/// (`instance_retention_conformance`) binds it.
// These four actions select facts supplied by the filesystem/OS environment;
// no shipping state transition implements the observation itself. The real
// sweeper produces those facts and `Decide` consumes them. Keep that boundary
// explicit rather than attaching a refinement to a function that does not own
// the transition.
#[cfg(any(test, debug_assertions))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ExactInstanceRetention",
    action = "SelectHeld",
    reason = "environment observation: advisory-lock contention supplies Held; the exhaustive \
              Tier-1 matrix projects that real decision input and validates its consumption"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ExactInstanceRetention",
    action = "SelectFree",
    reason = "environment observation: successful advisory-lock acquisition supplies Free; the \
              exhaustive Tier-1 matrix projects that real decision input and validates it"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ExactInstanceRetention",
    action = "SelectMalformed",
    reason = "environment observation: link/type/I/O failures supply Invalid; the exhaustive \
              Tier-1 matrix projects that fail-closed decision input and validates it"
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ExactInstanceRetention",
    action = "ObservePidAlive",
    reason = "environment observation: the OS liveness probe supplies this compatibility fact \
              only for a missing legacy lease; the Tier-1 matrix covers both values"
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn instance_retention_waivers() {}
