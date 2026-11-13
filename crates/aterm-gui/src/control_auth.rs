// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Default-on access control and artifact confinement for the per-user control socket.

use std::path::{Path, PathBuf};

use aterm_types::control_socket::{self, SocketDirective};
use aterm_uds::CtlStream;

#[cfg(unix)]
#[path = "control_auth_unix.rs"]
mod imp;
#[cfg(windows)]
#[path = "control_auth_win.rs"]
mod imp;

#[cfg(windows)]
pub(crate) use imp::ensure_control_dir;
#[cfg(not(windows))]
pub(crate) use imp::ensure_private_dir as ensure_control_dir;
#[cfg(unix)]
pub(crate) use imp::our_uid;
#[cfg(all(unix, any(target_os = "macos", test)))]
pub(crate) use imp::peer_uid;
pub(crate) use imp::pid_alive;
pub(crate) use imp::{
    SocketOwner, ensure_private_dir, lock_socket_file, peer_check, provision_token,
};

/// Atomically point the `latest` alias at this instance's socket ([`aterm_uds::latest::publish`]).
pub(crate) fn publish_latest_link(link: &Path, sock_path: &str) {
    aterm_uds::latest::publish(link, sock_path);
}

/// Generate 32 random bytes as 64 lowercase hex characters.
#[must_use]
pub(crate) fn random_token_hex() -> Option<String> {
    aterm_uds::rand::hex_token::<32>().ok()
}

/// Filename of the `latest` alias in the per-user socket directory.
pub(crate) const SOCK_FILE: &str = control_socket::LATEST_SOCK_FILE;

/// Subdirectory of the socket directory that confines `image`-verb PNG writes.
pub(crate) const IMAGES_DIR: &str = "images";

/// Server-auto-named captures live below this private child, separated from
/// caller-explicit filenames so retention can never delete an explicit target.
const AUTO_IMAGES_DIR: &str = "auto";

/// Subdirectory for video introspection recordings.
pub(crate) const VIDEO_DIR: &str = "video";
/// Visibility marker file created when a recording becomes non-abortable.
pub(crate) const VIDEO_PUBLISHED_FILE: &str = ".published";

/// Confinement failure outcome distinguishing resource refusal from an invalid path.
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

/// Run the calling test in a child process with `RLIMIT_NOFILE` constrained to `spare_descriptors`.
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
/// writes in the shared `images/` namespace.
const CAPTURE_LOCK_DIR: &str = ".capture-locks";
const CAPTURE_NAMESPACE_LEASE_FILE: &str = "explicit";

/// Hang detector for an explicit capture waiting for the previous reply's retention guard,
/// which is released at its client's acknowledgement.
const CAPTURE_NAMESPACE_HANG: std::time::Duration = std::time::Duration::from_secs(60);

/// Stable, collision-resistant identity for this server process instance.
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

/// Mint a server-unique filename for an omitted-name capture.
#[must_use]
pub(crate) fn automatic_capture_name(stem: &str) -> String {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    automatic_capture_name_for(stem, process_instance_id(), sequence)
}

/// Lease retaining ownership of a filesystem artifact across wire handoff.
#[derive(Debug)]
pub(crate) struct ArtifactPathLease {
    key: Option<PathBuf>,
    os_lock: Option<crate::native_document_host::HeldAdvisoryLock>,
    /// Capability local to this lease.
    video_sweep: Option<VideoRetentionSweep>,
    /// Automatic-image namespace GC is armed by both the target and its wire
    /// lease.
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

/// Serialize each bounded retention census with its mutations.
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
    /// admitted producer/reader lease.
    video_identity: Option<VideoLeaseIdentity>,
    /// Set at the marker visibility boundary or after a reader's final closed
    /// identity validation.
    video_sweep_requested: bool,
    /// A count-zero entry remains present while its capability-bound sweep runs.
    sweeping: bool,
    /// Reserved when this distinct video key first enters the registry.
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
    const fn arm_tombstone(&mut self) {
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
    let _initializing = INITIALIZING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(scheduler) = SCHEDULER.get() {
        return Some(scheduler);
    }
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
/// census to the single bounded cleanup worker.
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

/// Lease-registry entry snapshot for Tier-1 conformance tests.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ArtifactLeaseEntryForTest {
    pub(crate) count: usize,
    pub(crate) sweep_requested: bool,
    pub(crate) sweeping: bool,
    /// Whether the entry still holds its reserved retention admission.
    pub(crate) admission_held: bool,
}

/// Return the lease-registry entry for `key`, or `None` when unleased.
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

/// Park the process-wide artifact cleanup worker until the returned guard drops.
#[cfg(all(test, unix))]
pub(crate) fn hold_artifact_cleanup_for_test() -> ArtifactCleanupHold {
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    assert!(try_schedule_artifact_cleanup(ArtifactCleanupTask::Hold(
        std::sync::Arc::clone(&gate)
    )));
    gate.wait();
    ArtifactCleanupHold(gate)
}

/// Guard that releases the worker parked by [`hold_artifact_cleanup_for_test`].
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
    acquire_capture_name_lease_started(target, cancelled, std::time::Instant::now())
}

fn acquire_capture_name_lease_started(
    target: &ConfinedImage,
    cancelled: impl Fn() -> bool,
    started: std::time::Instant,
) -> std::io::Result<Option<ArtifactPathLease>> {
    acquire_capture_name_lease_with_wait_started(target, cancelled, CAPTURE_NAMESPACE_HANG, started)
}

#[cfg(test)]
pub(crate) fn acquire_capture_name_lease_with_wait(
    target: &ConfinedImage,
    cancelled: impl Fn() -> bool,
    explicit_wait: std::time::Duration,
) -> std::io::Result<Option<ArtifactPathLease>> {
    acquire_capture_name_lease_with_wait_started(
        target,
        cancelled,
        explicit_wait,
        std::time::Instant::now(),
    )
}

fn acquire_capture_name_lease_with_wait_started(
    target: &ConfinedImage,
    cancelled: impl Fn() -> bool,
    explicit_wait: std::time::Duration,
    waited_from: std::time::Instant,
) -> std::io::Result<Option<ArtifactPathLease>> {
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
        #[cfg(unix)]
        let mut file = target.pinned()?.open_directory_lock()?;
        #[cfg(windows)]
        let mut file = {
            let lock_dir = target
                .pinned()?
                .ensure_child(std::ffi::OsStr::new(CAPTURE_LOCK_DIR))?;
            lock_dir.open_namespace_lock(std::ffi::OsStr::new(CAPTURE_NAMESPACE_LEASE_FILE))?
        };
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
/// queued, written, consumed, and acknowledged.
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
    /// Arm a last-release video retention sweep on this lease.
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
    let is_symlink = metadata.file_type().is_symlink();
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        is_symlink || metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        is_symlink
    }
}

/// Test-only: a point between one of this module's checks and the re-check
/// that repeats it before the use.
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
    let pid = instance.strip_prefix('p')?.split_once('-')?.0;
    pid.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| pid.parse().ok())
        .flatten()
}

const INSTANCE_LEASE_FILE: &str = ".instance.lease";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstanceLeaseState {
    /// No lease file; PID liveness is the compatibility authority.
    Missing,
    /// The launch currently holds the advisory lease.
    Held,
    /// The lease exists and exclusive acquisition proved its launch exited.
    Acquirable,
    /// Symlink/reparse, wrong file type, or I/O error; fail closed and keep.
    Invalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstanceSweepDecision {
    Keep,
    Remove,
}

/// Decide whether an instance namespace directory should be kept or removed.
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
/// symlink/reparse point.
fn open_private_namespace_lock(
    namespace: &Path,
    file_name: &std::ffi::OsStr,
    create: bool,
) -> std::io::Result<std::fs::File> {
    let namespace = std::fs::canonicalize(namespace)?;
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
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
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
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
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
        if std::fs::canonicalize(&path)?.parent() != Some(namespace.as_path()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "namespace lock redirects outside its namespace",
            ));
        }
        if opened.permissions().mode() & 0o7777 != 0o600 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
    }
    #[cfg(not(unix))]
    if std::fs::canonicalize(&path)?.parent() != Some(namespace.as_path()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "namespace lock redirects outside its namespace",
        ));
    }
    Ok(file)
}

/// Open the launch-identity lease.
fn open_instance_lease(namespace: &Path, create: bool) -> std::io::Result<std::fs::File> {
    open_private_namespace_lock(namespace, std::ffi::OsStr::new(INSTANCE_LEASE_FILE), create)
}

/// Hold the current launch's lease until process exit.
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

/// Remove only namespaces proven abandoned.
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

    /// Consume one bounded slice of a persistent directory cursor.
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
            let legacy_pid_alive = lease_state == InstanceLeaseState::Missing && pid_alive(pid);
            if decide_instance_namespace_sweep(lease_state, legacy_pid_alive)
                == InstanceSweepDecision::Remove
            {
                #[cfg(not(windows))]
                let _acquired_lease = acquired_lease;
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

/// Resolve the private recording root for `instance` under `sock_dir`.
#[must_use]
pub(crate) fn video_instance_root_for(sock_dir: &Path, instance: &str) -> Option<PathBuf> {
    valid_instance_component(instance).then(|| sock_dir.join(VIDEO_DIR).join(instance))
}

/// Owned, pinned server-minted recording directory.
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
    /// wake.
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
    /// A tombstone arrived after the current persistent cursor began.
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

    /// Consume one bounded slice.
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

/// Closed identity checkpoint for a single frame in a private video bundle.
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

    /// Write one frame ordered ahead of the marker and return its [`VideoFrameSeal`].
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

    /// Write a batch member without syncing the recording directory yet.
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

    /// Sync batch members, publish the visibility marker, and arm retention.
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

    /// Atomically publish the visibility marker required by readers.
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

/// Create and pin a fresh server-named recording directory under `video/<instance>/`.
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

/// Confine an omitted-name `image`/`window` output to this launch's auto namespace.
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
    let seq = name.rsplit_once('-')?.1;
    seq.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| seq.parse().ok())
        .flatten()
}

/// Run the automatic-image retention sweep synchronously for tests.
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

/// Best-effort cleanup for a failed omitted-name image write.
pub(crate) fn cleanup_failed_automatic_image(target: &ConfinedImage) {
    if is_current_automatic_image(target)
        && let Some(pinned) = target.pinned.as_ref()
    {
        let _ = pinned.remove_file_if_exists(&target.file_name);
    }
}

/// Recordings kept on disk, including the one just created.
const VIDEO_KEEP: usize = 8;

/// Video retention (`prune_video_dirs`) deletes completed recordings beyond [`VIDEO_KEEP`],
/// never `fresh`, oldest first: server-named `rec-<sequence>-<collision>` names sort in
/// creation order within a namespace.
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
    const fn new(limit: usize) -> Self {
        Self {
            remaining: limit,
            scanned: 0,
            file_opens: 0,
            deletes: 0,
            renames: 0,
        }
    }

    const fn spend(&mut self, units: usize) -> bool {
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

/// Resolve and harden (`0700`) the per-user control socket directory.
#[must_use]
pub(crate) fn socket_dir() -> Option<PathBuf> {
    let dir = aterm_uds::control_socket_dir()?;
    ensure_private_dir(&dir).ok()?;
    Some(dir)
}

/// Create and return `<control dir>/<name>` with `0700` permissions.
pub(crate) fn private_subdir(name: &str) -> Option<PathBuf> {
    let dir = socket_dir()?.join(name);
    ensure_private_dir(&dir).ok()?;
    Some(dir)
}

/// Path to session `sid`'s file inside `dir`.
pub(crate) fn sid_file_in(dir: &Path, sid: &aterm_session::SessionId) -> Option<PathBuf> {
    crate::keeper_link::valid_sid(sid.as_str()).then(|| dir.join(sid.as_str()))
}

/// Provisioning plan for this instance's control socket.
#[derive(Clone)]
pub(crate) struct SocketPlan {
    /// Listening socket path.
    pub sock_path: String,
    /// Capability-token file path.
    pub token_path: PathBuf,
    /// Optional `latest` alias path to maintain.
    pub latest_link: Option<PathBuf>,
}

/// Resolution of how the control socket should be provisioned.
pub(crate) enum SocketResolution {
    /// Bind according to this plan.
    Enabled(SocketPlan),
    /// Disabled by CLI flag or env override.
    Disabled,
    /// No per-user directory or override could be resolved.
    NoDir,
    /// The per-user directory could not be made private (`0700`).
    DirUnusable { dir: PathBuf, error: String },
    /// The socket path exceeds [`MAX_SUN_PATH`].
    PathTooLong { path: String, limit: usize },
}

/// Maximum socket path byte length supported by `sockaddr_un` on this platform.
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

/// Whether `path` fits inside `sockaddr_un` ([`MAX_SUN_PATH`]).
#[must_use]
pub(crate) const fn sun_path_ok(path: &str) -> bool {
    path.len() <= MAX_SUN_PATH
}

/// Resolve the control socket plan for this process launch.
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

/// Remove sockets and token files belonging to dead PIDs in `dir`.
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

/// Summary of stale socket files removed or retained during a launch sweep.
#[cfg(any(windows, test))]
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SockSweep {
    /// Stale files removed.
    pub removed: Vec<String>,
    /// Stale files that could not be removed.
    pub failed: Vec<String>,
}

/// Remove zero-length `*.sock` files in `dir` at least `min_age` old with no listener.
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

/// Run [`sweep_stale_socket_files`] at launch over current and legacy socket directories.
#[cfg(windows)]
pub(crate) fn sweep_stale_socket_files_at_launch(sock_dir: &Path) {
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

/// Remove this instance's socket, token, and `latest` alias on graceful exit.
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

/// Parent directory of socket `path`.
#[must_use]
pub(crate) fn dir_of_socket(path: &str) -> PathBuf {
    Path::new(path)
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Capability-token path paired with socket `path`.
#[must_use]
pub(crate) fn token_path_for_socket(path: &str) -> PathBuf {
    let name = Path::new(path)
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    dir_of_socket(path).join(control_socket::token_name_for_sock(&name))
}

/// Read and trim the capability token paired with `sock_path`.
#[cfg(test)]
#[must_use]
pub(crate) fn read_token(sock_path: &str) -> Option<String> {
    let raw = std::fs::read_to_string(token_path_for_socket(sock_path)).ok()?;
    let t = raw.trim().to_string();
    if t.is_empty() { None } else { Some(t) }
}

/// Whether the control socket at `path` has a live owner: a held [`SocketOwner`] lock on its token
/// file answers yes with no connect, since a refusal never proves a socket stale (a stopped
/// owner's full queue refuses too) and a blocking connect to one can sleep. Otherwise a connect
/// decides, fail-safe true on an error that says neither refused nor absent.
#[must_use]
pub(crate) fn socket_is_live(path: &str) -> bool {
    use aterm_uds::ownerlock::{Owner, probe};
    use std::io::ErrorKind;
    if std::fs::symlink_metadata(path).is_ok() && probe(&token_path_for_socket(path)) == Owner::Live
    {
        return true;
    }
    match CtlStream::connect(path) {
        Ok(_) => true,
        Err(e) => !matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound),
    }
}

/// Non-blocking connection attempt to `path`.
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
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let kind = libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let kind = libc::SOCK_STREAM;
    let fd = aterm_uds::kept_out(|| {
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
        Ok(fd)
    })?;
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

/// [`socket_is_live`] for `aterm-<ourpid>.sock`, treating `EACCES`/`EPERM` as dead.
#[must_use]
pub(crate) fn socket_is_live_per_instance(path: &str) -> bool {
    CtlStream::connect(path).is_ok()
}

/// Action to take on an existing socket path before binding.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BindAction {
    /// Unlink any stale file and bind.
    RemoveAndBind,
    /// Refuse to overwrite a live socket.
    RefuseLiveSocket,
}

/// Decide whether to remove and bind a socket path based on liveness.
#[must_use]
pub(crate) const fn decide_bind(is_live: bool) -> BindAction {
    if is_live {
        BindAction::RefuseLiveSocket
    } else {
        BindAction::RemoveAndBind
    }
}

/// Result of validating a connection's initial auth line.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AuthOutcome {
    /// Authenticated, with optional trailing inline command.
    Ok(Option<String>),
    /// Authentication failed.
    Denied,
}

/// Process-local 32-byte hex token minted on first use for harness connections.
pub(crate) fn harness_token() -> Option<&'static str> {
    static TOKEN: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    TOKEN.get_or_init(random_token_hex).as_deref()
}

/// Validate `line` against [`harness_token`].
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

/// Validate a connection's first line against `expected` in constant time.
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

/// Validated image output path confined to the `images/` subdirectory.
#[derive(Clone, Debug)]
pub(crate) struct ConfinedImage {
    /// Canonical `images/` directory.
    pub dir: PathBuf,
    /// Validated single-component filename.
    pub file_name: std::ffi::OsString,
    /// Every absolute ancestor plus the exact output directory, retained from
    /// confinement until the encode worker commits or refuses the reply.
    pinned: Option<crate::pinned_dir::PinnedDir>,
    /// The target and its path lease share this kick, so stale
    /// namespace GC starts only after the final retained path capability closes.
    deferred_cleanup: Option<std::sync::Arc<DeferredImageCleanup>>,
}

impl ConfinedImage {
    /// Target-free sentinel for an inline in-memory image reply.
    pub(crate) const fn in_memory() -> Self {
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

    /// Full path for logging and reply formatting.
    #[must_use]
    pub(crate) fn display_path(&self) -> PathBuf {
        self.dir.join(&self.file_name)
    }

    /// Arm best-effort automatic-image retention after all handles close.
    pub(crate) fn arm_automatic_prune(&self) {
        if let Some(cleanup) = self.deferred_cleanup.as_ref() {
            cleanup.arm_image_prune();
        }
    }

    /// Write `bytes` relative to the retained directory handle.
    #[cfg(test)]
    pub(crate) fn write_private(
        &self,
        bytes: &[u8],
    ) -> std::io::Result<crate::pinned_dir::PinnedFile> {
        self.write_private_authorized(bytes, || true)
    }

    /// Write `bytes` and run `authorize` before the final commit.
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

    /// Verify that all ancestor components still resolve to the retained directory identity.
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
    let raw_candidate = if req.is_absolute() {
        req.to_path_buf()
    } else {
        canon_images.join(req)
    };

    let lexical = lexically_normalize(&raw_candidate)?;

    let file_name = lexical.file_name()?;
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

/// Confine a discovery-graph socket path to `sock_dir`.
#[must_use]
pub(crate) fn confine_proxy_sock(sock_dir: &Path, requested: &str) -> Option<String> {
    let canon_dir = std::fs::canonicalize(sock_dir).ok()?;
    let req = Path::new(requested);
    if !req.is_absolute() {
        return None;
    }
    let lexical = lexically_normalize(req)?;
    let file_name = lexical.file_name()?;
    if !crate::pinned_dir::is_one_component(file_name) {
        return None;
    }
    let canon_parent = std::fs::canonicalize(lexical.parent()?).ok()?;
    if canon_parent != canon_dir {
        return None;
    }
    let resolved = canon_dir.join(file_name);
    if let Ok(md) = std::fs::symlink_metadata(&resolved)
        && md.file_type().is_symlink()
    {
        return None;
    }
    Some(resolved.to_string_lossy().into_owned())
}

/// Lexically resolve `.`/`..`/`//` in an absolute path without touching the filesystem.
fn lexically_normalize(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut out: Vec<Component> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) | None => return None,
                _ => out.push(comp),
            },
            other => out.push(other),
        }
    }
    Some(out.iter().collect())
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

#[cfg(test)]
#[path = "control_auth_tests.rs"]
mod tests;
