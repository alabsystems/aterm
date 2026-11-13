// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The warm-up: a deliberate pre-prompt at a moment a human is at the screen.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use aterm_types::MutexExt as _;

#[cfg(any(target_os = "macos", test))]
use aterm_containment::consent;
use aterm_containment::consent::Folder;

/// Bound on the warm-up result queue. A pass posts at most `2n + 1` messages for its `n`
/// de-duplicated folders (`n <= Folder::ALL.len()`), so the bound is never reached; it exists
/// because the worker must not block on its output.
const RESULT_QUEUE_CAP: usize = 16;

/// Whether a gated pass may ask its next folder (`docs/DESIGN-system-state-2026-10-21.md` §6:
/// steps park when the person leaves the tab). The window holds it open while System State
/// is the visible view and shuts it when the person leaves; the worker waits at it before
/// each folder. A folder already asked stays asked — macOS's dialog cannot be taken back —
/// but the next waits. Only the launch pass is gated; the owner's own pass from the
/// Security page never is.
#[derive(Debug, Default)]
struct WarmupGateState {
    open: bool,
    retired: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct WarmupGate(Arc<(Mutex<WarmupGateState>, Condvar)>);

impl WarmupGate {
    /// A gate that starts open: a pass starts only at its step moment.
    pub(crate) fn open() -> Self {
        let gate = Self::default();
        gate.set(true);
        gate
    }

    /// Open or shut the gate; a worker waiting at it goes on once it opens.
    pub(crate) fn set(&self, open: bool) {
        let (state, changed) = &*self.0;
        let mut state = state.lock_or_recover();
        let open = open && !state.retired;
        if state.open != open {
            state.open = open;
            changed.notify_all();
        }
    }

    /// Withdraw this automatic pass permanently. Its current syscall may
    /// finish, but no later folder can start, even after a newer approval.
    pub(crate) fn retire(&self) {
        let (state, changed) = &*self.0;
        let mut state = state.lock_or_recover();
        state.retired = true;
        state.open = false;
        changed.notify_all();
    }

    pub(crate) fn is_retired(&self) -> bool {
        self.0.0.lock_or_recover().retired
    }

    /// Whether the gate is open.
    #[cfg(test)]
    pub(crate) fn is_open(&self) -> bool {
        self.0.0.lock_or_recover().open
    }

    /// Wait until the gate is open (the worker; it may wait for the life of the process,
    /// as a listing parked in `tccd` may).
    fn wait_open(&self) -> bool {
        let (state, changed) = &*self.0;
        let mut open = state.lock_or_recover();
        while !open.open && !open.retired {
            open = changed
                .wait(open)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        !open.retired
    }
}

/// What one folder's listing attempt returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadDirOutcome {
    /// The directory listed (an entry read, or empty): the access was permitted.
    #[cfg(any(target_os = "macos", test))]
    Listed,
    /// The listing failed with this raw errno.
    #[cfg(any(target_os = "macos", test))]
    Failed(i32),
    /// The probe was not performed: a headless instance, a unit test, an executable outside a
    /// `.app`, or off macOS.
    Refused,
}

/// One folder's state on the Security panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum WarmupRow {
    /// Nothing has been asked about this folder in this process.
    #[default]
    Unknown,
    /// The worker is inside this folder's listing syscall, perhaps parked behind a system dialog
    /// with no timeout, so the row can stay here indefinitely.
    Asking,
    /// The listing succeeded from this process.
    #[cfg(any(target_os = "macos", test))]
    Allowed,
    /// `EPERM`, the TCC refusal: macOS does not ask again for this folder.
    Denied,
    /// Some other errno.
    #[cfg(any(target_os = "macos", test))]
    Error,
}

impl WarmupRow {
    /// The report and panel spelling, which the `privacy` verb's folder rows read.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Asking => "asking",
            #[cfg(any(target_os = "macos", test))]
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            #[cfg(any(target_os = "macos", test))]
            Self::Error => "error",
        }
    }
}

/// What one listing means for a folder's row: the one place the mapping lives.
pub(crate) const fn fold_read_dir(folder: Folder, outcome: ReadDirOutcome) -> WarmupRow {
    #[cfg(not(any(target_os = "macos", test)))]
    let _ = folder;
    match outcome {
        #[cfg(any(target_os = "macos", test))]
        ReadDirOutcome::Listed => WarmupRow::Allowed,
        #[cfg(any(target_os = "macos", test))]
        ReadDirOutcome::Failed(consent::ERRNO_EPERM) => WarmupRow::Denied,
        #[cfg(any(target_os = "macos", test))]
        ReadDirOutcome::Failed(consent::ERRNO_ENOENT) if folder.absent_means_allowed() => {
            WarmupRow::Allowed
        }
        #[cfg(any(target_os = "macos", test))]
        ReadDirOutcome::Failed(_) => WarmupRow::Error,
        ReadDirOutcome::Refused => WarmupRow::Unknown,
    }
}

/// The one OS question this module asks, injected like every consent probe
/// (`consent_state::ConsentProbes`).
#[derive(Clone, Copy)]
pub(crate) struct WarmupProbe {
    /// Lists one already-resolved directory. May park indefinitely.
    read_dir: fn(&Path) -> ReadDirOutcome,
    /// Whether this is the live arm.
    live: bool,
}

impl WarmupProbe {
    /// The windowed instance's arm.
    pub(crate) const fn live() -> Self {
        Self {
            read_dir: live_read_dir,
            live: true,
        }
    }

    /// The headless / unit-test arm: no syscall.
    pub(crate) const fn inert() -> Self {
        Self {
            read_dir: inert_read_dir,
            live: false,
        }
    }

    /// `live()` for a windowed instance, `inert()` for a headless one.
    pub(crate) const fn for_instance(headless: bool) -> Self {
        if headless {
            Self::inert()
        } else {
            Self::live()
        }
    }

    /// Whether this instance's arm can reach the filesystem at all.
    #[cfg(test)]
    pub(crate) const fn is_live(self) -> bool {
        self.live
    }
}

/// The inert arm.
const fn inert_read_dir(_path: &Path) -> ReadDirOutcome {
    ReadDirOutcome::Refused
}

/// The live arm: one listing of an already-resolved path.
#[cfg(target_os = "macos")]
fn live_read_dir(path: &Path) -> ReadDirOutcome {
    let Ok(exe) = std::env::current_exe() else {
        return ReadDirOutcome::Refused;
    };
    if !consent::path_is_in_app_bundle(&exe) {
        return ReadDirOutcome::Refused;
    }
    match std::fs::read_dir(path) {
        Ok(mut entries) => match entries.next() {
            None | Some(Ok(_)) => ReadDirOutcome::Listed,
            Some(Err(err)) => ReadDirOutcome::Failed(err.raw_os_error().unwrap_or_default()),
        },
        Err(err) => ReadDirOutcome::Failed(err.raw_os_error().unwrap_or_default()),
    }
}

/// Off macOS there is no TCC: the live arm refuses like the inert one.
#[cfg(not(target_os = "macos"))]
fn live_read_dir(_path: &Path) -> ReadDirOutcome {
    ReadDirOutcome::Refused
}

/// One message from a warm-up worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WarmupProgress {
    /// The worker is about to enter the listing syscall for this folder.
    Asking { generation: u64, folder: Folder },
    /// One folder answered.
    Answered {
        generation: u64,
        folder: Folder,
        outcome: ReadDirOutcome,
    },
    /// The pass walked every folder and the worker is exiting.
    Finished {
        generation: u64,
        elapsed_ms: u128,
        answers: Vec<(Folder, ReadDirOutcome)>,
    },
}

/// What an *Ask for folder access now* gesture did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartOutcome {
    /// A worker was spawned and the apply hold is armed.
    Started,
    /// A worker is already walking.
    AlreadyLive,
    /// Another operation owns the process's macOS dialog slot.
    DialogUp,
    /// `[privacy] enabled = false`, `warmup = "never"`, or a headless instance.
    Refused,
    /// No configured folder name resolved to a path: every name was unknown, or `$HOME` is unset.
    NoFolders,
    /// The OS refused the thread.
    SpawnFailed,
}

impl StartOutcome {
    /// Why a confirmed press started nothing, in the owner's words; `None` while a worker walks,
    /// this press's or one already out.
    pub(crate) const fn refusal(self) -> Option<&'static str> {
        match self {
            Self::Started | Self::AlreadyLive => None,
            Self::DialogUp => Some("another macOS dialog is already up; try again after it closes"),
            Self::Refused => Some(
                "the privacy checks or the warm-up are off, or this instance has no window to \
                 ask in",
            ),
            Self::NoFolders => Some(
                "no folder in [privacy] warmup_folders resolved to a path (an unknown name, or \
                 no home folder)",
            ),
            Self::SpawnFailed => Some("macOS refused the thread it needs"),
        }
    }
}

/// The live pass's bookkeeping.
#[derive(Clone, Copy, Debug)]
struct LivePass {
    /// Which pass this is; stale messages are dropped against it.
    generation: u64,
    /// When the gesture happened. The apply hold is measured from here.
    started_at: Instant,
    /// Hard cap on the in-place-apply hold, from `[privacy] warmup_hold_ms`.
    hold_cap: Duration,
}

/// The instance's warm-up state: the rows, the live pass and the result queue's receiving end.
pub(crate) struct WarmupState {
    /// This instance's listing arm.
    probe: WarmupProbe,
    /// The live pass, or `None` when idle.
    live: Option<LivePass>,
    /// One row per walked folder, in walk order.
    rows: Vec<(Folder, WarmupRow)>,
    /// Wall time of the last completed pass.
    last_pass_ms: Option<u128>,
    /// Monotonic pass counter.
    generation: u64,
    /// Receiving end of the live pass's bounded queue.
    rx: Option<Receiver<WarmupProgress>>,
}

impl WarmupState {
    /// Wire the arm for this instance.
    pub(crate) const fn new(headless: bool) -> Self {
        Self {
            probe: WarmupProbe::for_instance(headless),
            live: None,
            rows: Vec::new(),
            last_pass_ms: None,
            generation: 0,
            rx: None,
        }
    }

    /// The headless / unit-test instance.
    pub(crate) fn inert() -> Self {
        Self::new(true)
    }

    /// An instance whose worker answers every folder with `read_dir` (test-only).
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn with_read_dir_for_test(read_dir: fn(&Path) -> ReadDirOutcome) -> Self {
        let mut state = Self::inert();
        state.probe = WarmupProbe {
            read_dir,
            live: false,
        };
        state
    }

    /// Whether this instance's probe can reach the filesystem.
    #[cfg(test)]
    pub(crate) const fn probe_is_live(&self) -> bool {
        self.probe.is_live()
    }

    /// Start one pass over `folders`, already-resolved paths.
    #[cfg(test)]
    pub(crate) fn start<P>(
        &mut self,
        folders: &[(Folder, PathBuf)],
        hold_cap: Duration,
        poke: P,
    ) -> StartOutcome
    where
        P: Fn() + Send + 'static,
    {
        self.start_gated(folders, hold_cap, None, poke)
    }

    /// `Self::start`; with a `gate`, the folders' looks run together, each waiting at it: the
    /// launch pass parks while the person is away from System State.
    pub(crate) fn start_gated<P>(
        &mut self,
        folders: &[(Folder, PathBuf)],
        hold_cap: Duration,
        gate: Option<WarmupGate>,
        poke: P,
    ) -> StartOutcome
    where
        P: Fn() + Send + 'static,
    {
        self.drain();
        if self.live.is_some() {
            return StartOutcome::AlreadyLive;
        }
        let walk = dedup_folders(folders);
        if walk.is_empty() {
            return StartOutcome::NoFolders;
        }
        let generation = self.generation.wrapping_add(1);
        let (tx, rx) = std::sync::mpsc::sync_channel::<WarmupProgress>(RESULT_QUEUE_CAP);
        let probe = self.probe;
        let spawned = std::thread::Builder::new()
            .name("consent-warmup".into())
            .spawn(move || run_pass(generation, &walk, probe, gate.as_ref(), &tx, &poke));
        match spawned {
            Ok(_handle) => {}
            Err(err) => {
                aterm_log::warn!("consent warm-up could not start its worker: {err}");
                return StartOutcome::SpawnFailed;
            }
        }
        self.generation = generation;
        self.rows = folders_to_unknown_rows(folders);
        self.rx = Some(rx);
        self.live = Some(LivePass {
            generation,
            started_at: Instant::now(),
            hold_cap,
        });
        StartOutcome::Started
    }

    /// Fold every queued message into the rows; returns how many were applied.
    pub(crate) fn drain(&mut self) -> usize {
        let mut applied = 0usize;
        loop {
            let Some(rx) = self.rx.as_ref() else {
                return applied;
            };
            match rx.try_recv() {
                Ok(message) => {
                    if self.apply(message) {
                        applied = applied.saturating_add(1);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return applied,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.end_pass();
                    return applied;
                }
            }
        }
    }

    /// Fold one message. Returns whether it was for the live pass.
    fn apply(&mut self, message: WarmupProgress) -> bool {
        let generation = match &message {
            WarmupProgress::Asking { generation, .. }
            | WarmupProgress::Answered { generation, .. }
            | WarmupProgress::Finished { generation, .. } => *generation,
        };
        if self.live.is_none_or(|live| live.generation != generation) {
            return false;
        }
        match message {
            WarmupProgress::Asking { folder, .. } => self.set_row(folder, WarmupRow::Asking),
            WarmupProgress::Answered {
                folder, outcome, ..
            } => self.set_row(folder, fold_read_dir(folder, outcome)),
            WarmupProgress::Finished {
                elapsed_ms,
                answers,
                ..
            } => {
                for (folder, outcome) in answers {
                    self.set_row(folder, fold_read_dir(folder, outcome));
                }
                self.last_pass_ms = Some(elapsed_ms);
                self.end_pass();
            }
        }
        true
    }

    /// Retire the live pass. A row still `asking` becomes `unknown`: nobody learned the answer,
    /// which is not a denial.
    fn end_pass(&mut self) {
        self.live = None;
        self.rx = None;
        for (_, row) in &mut self.rows {
            if *row == WarmupRow::Asking {
                *row = WarmupRow::Unknown;
            }
        }
    }

    fn set_row(&mut self, folder: Folder, row: WarmupRow) {
        if let Some(slot) = self
            .rows
            .iter_mut()
            .find_map(|(name, slot)| (*name == folder).then_some(slot))
        {
            *slot = row;
        }
    }

    /// The latest pass's number: the rows are its rows.
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether a pass is walking right now.
    pub(crate) const fn is_live(&self) -> bool {
        self.live.is_some()
    }

    /// The rows, in walk order.
    pub(crate) fn rows(&self) -> &[(Folder, WarmupRow)] {
        &self.rows
    }

    /// Wall time of the last completed pass, for the `privacy` verb's `warmup_last_ms=`.
    pub(crate) const fn last_pass_ms(&self) -> Option<u128> {
        self.last_pass_ms
    }

    /// Arm a hold with no worker behind it (test-only).
    #[cfg(all(test, unix))]
    pub(crate) fn arm_hold_for_test(&mut self, started_at: Instant, hold_cap: Duration) {
        self.generation = self.generation.wrapping_add(1);
        self.live = Some(LivePass {
            generation: self.generation,
            started_at,
            hold_cap,
        });
    }

    /// The in-place-apply hold: true while a warm-up worker is live and the hold has not reached
    /// its cap.
    pub(crate) fn holds_automatic_apply(&self, now: Instant) -> bool {
        self.live
            .is_some_and(|live| hold_active(live.started_at, live.hold_cap, now))
    }
}

impl Default for WarmupState {
    /// Default-safe: the arm that cannot reach the OS.
    fn default() -> Self {
        Self::inert()
    }
}

impl std::fmt::Debug for WarmupState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WarmupState")
            .field("probe_live", &self.probe.live)
            .field("live", &self.live.is_some())
            .field("rows", &self.rows.len())
            .field("last_pass_ms", &self.last_pass_ms)
            .finish()
    }
}

/// Whether a hold armed at `started_at` with cap `hold_cap` is still active at `now`.
pub(crate) fn hold_active(started_at: Instant, hold_cap: Duration, now: Instant) -> bool {
    now.saturating_duration_since(started_at) < hold_cap
}

/// The walk list: the configured pairs with repeats removed (a repeat would raise the same
/// dialog twice), order preserved.
fn dedup_folders(folders: &[(Folder, PathBuf)]) -> Vec<(Folder, PathBuf)> {
    let mut seen: Vec<Folder> = Vec::new();
    let mut walk: Vec<(Folder, PathBuf)> = Vec::new();
    for (folder, path) in folders {
        if seen.contains(folder) {
            continue;
        }
        seen.push(*folder);
        walk.push((*folder, path.clone()));
    }
    walk
}

/// The row vector a fresh pass starts from: every configured folder, `unknown`.
fn folders_to_unknown_rows(folders: &[(Folder, PathBuf)]) -> Vec<(Folder, WarmupRow)> {
    dedup_folders(folders)
        .into_iter()
        .map(|(folder, _)| (folder, WarmupRow::Unknown))
        .collect()
}

/// Queue one message, dropped when the queue is full (never block, never coalesce), and poke
/// the event loop either way: the drain discovers the messages and, at the end, the disconnect.
fn post(tx: &SyncSender<WarmupProgress>, poke: &(impl Fn() + ?Sized), message: WarmupProgress) {
    match tx.try_send(message) {
        Ok(()) | Err(TrySendError::Disconnected(_)) => {}
        Err(TrySendError::Full(dropped)) => {
            aterm_log::debug!("consent warm-up dropped a full result queue's message: {dropped:?}");
        }
    }
    poke();
}

/// Look at the folders: the owner's pass in sequence; the launch pass (`gate`, which follows a
/// Full Disk Access page not answered with a grant) all at once ([`look_together`]). Each listing
/// may park indefinitely behind a system dialog, so this runs on a detached thread nothing joins.
fn run_pass(
    generation: u64,
    walk: &[(Folder, PathBuf)],
    probe: WarmupProbe,
    gate: Option<&WarmupGate>,
    tx: &SyncSender<WarmupProgress>,
    poke: &(impl Fn() + ?Sized),
) {
    let started = Instant::now();
    let answers = match gate {
        Some(gate) => look_together(generation, walk, probe, gate, tx, poke),
        None => walk
            .iter()
            .map(|(folder, path)| {
                post(
                    tx,
                    poke,
                    WarmupProgress::Asking {
                        generation,
                        folder: *folder,
                    },
                );
                let outcome = (probe.read_dir)(path);
                post(
                    tx,
                    poke,
                    WarmupProgress::Answered {
                        generation,
                        folder: *folder,
                        outcome,
                    },
                );
                (*folder, outcome)
            })
            .collect(),
    };
    post(
        tx,
        poke,
        WarmupProgress::Finished {
            generation,
            elapsed_ms: started.elapsed().as_millis(),
            answers,
        },
    );
}

/// One folder's look on the launch pass, as its thread reports it to the pass.
enum Look {
    Asking(Folder),
    Answered(Folder, ReadDirOutcome),
}

/// The launch pass's looks, each on a thread of its own and each waiting at `gate` (the launch
/// pass asks nothing while the person is away from System State): macOS queues their questions
/// back to back, so the next comes up as the last is answered instead of after the pass's own
/// round trip. The answers come back in `walk`'s order; a look its gate retired never asks.
fn look_together(
    generation: u64,
    walk: &[(Folder, PathBuf)],
    probe: WarmupProbe,
    gate: &WarmupGate,
    tx: &SyncSender<WarmupProgress>,
    poke: &(impl Fn() + ?Sized),
) -> Vec<(Folder, ReadDirOutcome)> {
    let (looked, looks) = std::sync::mpsc::channel::<Look>();
    let mut answers = std::thread::scope(|scope| {
        for (folder, path) in walk {
            let looked = looked.clone();
            let gate = gate.clone();
            let read_dir = probe.read_dir;
            let folder = *folder;
            scope.spawn(move || {
                if !gate.wait_open() {
                    return;
                }
                let _ = looked.send(Look::Asking(folder));
                let _ = looked.send(Look::Answered(folder, read_dir(path)));
            });
        }
        drop(looked);
        let mut answers = Vec::with_capacity(walk.len());
        for look in looks {
            match look {
                Look::Asking(folder) => {
                    post(tx, poke, WarmupProgress::Asking { generation, folder });
                }
                Look::Answered(folder, outcome) => {
                    answers.push((folder, outcome));
                    post(
                        tx,
                        poke,
                        WarmupProgress::Answered {
                            generation,
                            folder,
                            outcome,
                        },
                    );
                }
            }
        }
        answers
    });
    answers.sort_by_key(|(folder, _)| walk.iter().position(|(walked, _)| walked == folder));
    answers
}

#[cfg(test)]
#[path = "consent_warmup_tests.rs"]
mod tests;
