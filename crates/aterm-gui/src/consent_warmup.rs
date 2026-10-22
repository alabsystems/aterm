// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The warm-up: a deliberate pre-prompt at a moment a human is at the screen.
//!
//! Two callers start a pass: the owner's *Ask for folder access now* on the Security panel and
//! the window's launch pass (`crate::consent_launch`, `[privacy] warmup = "automatic"`), which
//! walks at a quiet moment while Full Disk Access reads off once the launch hand-over was left
//! without action. Either way a detached worker lists each configured folder in sequence (because
//! simultaneous system modals stack badly) and records what the filesystem said: `Ok`, `EPERM(1)`
//! or another errno. aterm renders, asks and answers nothing: it makes an ordinary file access at
//! a moment a human is at the screen, so macOS asks its question then rather than inside an
//! agent's blocked syscall.
//!
//! `tccd` holds the calling syscall until a human answers the dialog, with no timeout, `EINTR` or
//! cancel, so a worker in [`std::fs::read_dir`] can stay parked for the life of the process. The
//! worker is therefore spawned detached and its `JoinHandle` dropped on the spot, making an
//! event-loop join structurally impossible. Results return over a bounded channel the producer
//! `try_send`s into and drops on `Full`, plus a payload-free event-loop poke; the channel's
//! `Disconnected` edge is the authoritative end-of-pass signal.
//!
//! The folders arrive resolved from `aterm_containment::consent::folder_paths`; this file writes
//! no protected-folder path literal (the aterm-census guard `protected-folder`).
//!
//! Default `warmup = "automatic"` (`"on-request"` keeps only the Security panel's gesture,
//! `"never"` neither); there is no environment variable, no `VERBS` row and no dispatch arm, so
//! no session can trigger a consent-raising pass.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::time::{Duration, Instant};

#[cfg(any(target_os = "macos", test))]
use aterm_containment::consent;
use aterm_containment::consent::Folder;

/// Bound on the warm-up result queue. A pass posts at most `2n + 1` messages for its `n`
/// de-duplicated folders (`n <= Folder::ALL.len()`), so the bound is never reached; it exists
/// because the worker must not block on its output. Overflow drops, never coalesces: the
/// pass's final message repairs a dropped one, and the channel's `Disconnected` edge a
/// dropped final message.
const RESULT_QUEUE_CAP: usize = 16;

// The pure fold: what one directory listing means

/// What one folder's listing attempt returned. `Failed` carries the raw errno: only
/// [`fold_read_dir`] decides what it means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadDirOutcome {
    /// The directory listed (an entry read, or empty): the access was permitted.
    #[cfg(any(target_os = "macos", test))]
    Listed,
    /// The listing failed with this raw errno.
    #[cfg(any(target_os = "macos", test))]
    Failed(i32),
    /// The probe was not performed: a headless instance, a unit test, an executable outside a
    /// `.app`, or off macOS. Never rendered as a denial.
    Refused,
}

/// One folder's state on the Security panel. `Unknown` is what a folder reads before
/// anything asked, and again in a successor process.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum WarmupRow {
    /// Nothing has been asked about this folder in this process.
    #[default]
    Unknown,
    /// The worker is inside this folder's listing syscall, perhaps parked behind a system
    /// dialog with no timeout, so the row can stay here indefinitely.
    Asking,
    /// The listing succeeded from this process.
    #[cfg(any(target_os = "macos", test))]
    Allowed,
    /// `EPERM(1)` — the TCC refusal. macOS is not asking again for this folder.
    Denied,
    /// Some other errno. Not a consent verdict; the folder may not exist, or
    /// the volume may be gone.
    #[cfg(any(target_os = "macos", test))]
    Error,
}

impl WarmupRow {
    /// The report spelling, which is also the panel spelling: the `privacy`
    /// verb's folder rows read it.
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

/// What one listing means for a folder's row: the one place the mapping lives, so a test
/// can drive it.
///
/// A TCC refusal is `EPERM`; `EACCES` is an ordinary Unix permission problem and reads as
/// `Error`, so a `chmod` is never reported as a privacy denial (design §1.5). `ENOENT`
/// depends on the item ([`Folder::absent_means_allowed`]): a missing home folder is `Error`,
/// but being told the app-data item's container is absent proves the look was permitted,
/// since `tccd` decides before the lookup can report a missing name.
pub(crate) const fn fold_read_dir(folder: Folder, outcome: ReadDirOutcome) -> WarmupRow {
    // Off macOS every answer is `Refused`, whichever folder asked.
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

// The injected listing probe

/// The one OS question this module asks, injected like every consent probe
/// (`control_privacy::ConsentProbes`).
///
/// * [`WarmupProbe::live`]: a windowed instance; a real directory listing.
/// * [`WarmupProbe::inert`]: a headless instance and every unit test; answers
///   [`ReadDirOutcome::Refused`] without a syscall.
///
/// The live arm also re-checks the in-bundle guard on every call, a deliberate second gate
/// as in `aterm_containment::consent`: a test binary that touches `WindowServer` can make
/// `tccd` read its whole `target/debug/deps`.
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
fn inert_read_dir(_path: &Path) -> ReadDirOutcome {
    ReadDirOutcome::Refused
}

/// The live arm: one listing of an already-resolved path.
///
/// It re-applies the in-bundle guard first (design §3.3 guardrail 1), so a test binary that
/// acquired the live arm still cannot park a system modal. It reads the first entry because
/// the refusal surfaces at the first `readdir`, not at `opendir`; an empty directory reads
/// as `Listed`.
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

// Worker → main-thread messages

/// One message from a warm-up worker.
///
/// Every variant carries the pass `generation` so a message from a worker this
/// instance has already retired cannot rewrite a newer pass's rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WarmupProgress {
    /// The worker is about to enter the listing syscall for this folder. Posted before the
    /// call, which may never return.
    Asking { generation: u64, folder: Folder },
    /// One folder answered.
    Answered {
        generation: u64,
        folder: Folder,
        outcome: ReadDirOutcome,
    },
    /// The pass walked every folder and the worker is exiting. Carries the
    /// whole pass's answers, so one dropped [`Self::Answered`] cannot leave a
    /// row stuck at [`WarmupRow::Asking`].
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
    /// A worker is already walking. A deliberate no-op: a second worker would stack a second
    /// system modal behind the first.
    AlreadyLive,
    /// `[privacy] enabled = false`, `warmup = "never"`, or a headless instance. Nothing was
    /// asked.
    Refused,
    /// No configured folder name resolved to a path — every name was unknown,
    /// or `$HOME` is unset.
    NoFolders,
    /// The OS refused the thread. Nothing was started.
    SpawnFailed,
}

impl StartOutcome {
    /// Why a confirmed press started nothing, in the owner's words — `None`
    /// when a worker is walking, this press's or one already out.
    pub(crate) const fn refusal(self) -> Option<&'static str> {
        match self {
            Self::Started | Self::AlreadyLive => None,
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

// The instance's warm-up state

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
///
/// Instance-owned, with NO process-global anywhere in this module. That is
/// what makes §3.5's successor rule structural rather than a cleanup step: an
/// in-place apply builds a fresh `App`, which builds a fresh `WarmupState`, so
/// every row is `unknown` again and nothing claims anything about a modal that
/// may still be on the screen.
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
    pub(crate) fn new(headless: bool) -> Self {
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

    /// An instance whose worker answers every folder with `read_dir`: a stand-in that
    /// reaches no filesystem, for the window's tests.
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

    /// Start one pass over `folders`, already-resolved paths. `hold_cap` is
    /// `[privacy] warmup_hold_ms`, already clamped; `poke`, the event-loop wake, runs after every
    /// queued message. A second call while a pass is live returns [`StartOutcome::AlreadyLive`]
    /// and changes nothing.
    pub(crate) fn start<P>(
        &mut self,
        folders: &[(Folder, PathBuf)],
        hold_cap: Duration,
        poke: P,
    ) -> StartOutcome
    where
        P: Fn() + Send + 'static,
    {
        // Drain first: a pass that finished while nothing was looking must not
        // make the next gesture a no-op.
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
            .spawn(move || run_pass(generation, &walk, probe, &tx, &poke));
        // Detached on purpose: the handle is never stored, so nothing can join a thread that may
        // be parked in `tccd` with no timeout.
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

    /// Fold every queued message into the rows; returns how many were applied. `Disconnected`
    /// (the worker exited, dropping its sender) ends the pass, and a full queue cannot drop it,
    /// so a pass ends even if every message it sent was lost.
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

    /// Whether a pass is walking right now. Not drained — call [`Self::drain`]
    /// first when the answer must be fresh.
    pub(crate) const fn is_live(&self) -> bool {
        self.live.is_some()
    }

    /// The rows, in walk order.
    pub(crate) fn rows(&self) -> &[(Folder, WarmupRow)] {
        &self.rows
    }

    /// Wall time of the last completed pass, for the `privacy` verb's
    /// `warmup_last_ms=`.
    pub(crate) const fn last_pass_ms(&self) -> Option<u128> {
        self.last_pass_ms
    }

    /// Arm a hold with no worker behind it, so a unit test can drive the automatic-apply gate
    /// without spawning anything.
    #[cfg(all(test, unix))]
    pub(crate) fn arm_hold_for_test(&mut self, started_at: Instant, hold_cap: Duration) {
        self.generation = self.generation.wrapping_add(1);
        self.live = Some(LivePass {
            generation: self.generation,
            started_at,
            hold_cap,
        });
    }

    /// The in-place-apply hold (design §3.5): true while a warm-up worker is live and the hold has
    /// not reached its cap. Read only on the automatic lanes, as its own ladder fact
    /// (`App::update_apply_warmup_holds`) that no phase relaxes, so the last phase cannot land on
    /// top of the dialog; a manual `aterm ctl update apply` is never held.
    ///
    /// It is not "defer while agents are live" (design §3.9 refuses that: one long-lived session
    /// would pin an old build). The cap also makes a missed drain harmless: a pass whose messages
    /// were never folded still stops holding at the cap.
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

// Pure helpers

/// Whether a hold armed at `started_at` with cap `hold_cap` is still active at
/// `now`. Saturating, so a clock that went backwards holds rather than panics,
/// and a zero cap never holds at all.
pub(crate) fn hold_active(started_at: Instant, hold_cap: Duration, now: Instant) -> bool {
    now.saturating_duration_since(started_at) < hold_cap
}

/// The walk list: the configured pairs with repeats removed (a repeat would raise the same
/// dialog twice), order preserved. This is what bounds a pass under [`RESULT_QUEUE_CAP`].
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

// The worker

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

/// Walk the folders in sequence. Each listing may park indefinitely behind a system dialog,
/// so this runs on a detached thread nothing joins.
fn run_pass(
    generation: u64,
    walk: &[(Folder, PathBuf)],
    probe: WarmupProbe,
    tx: &SyncSender<WarmupProgress>,
    poke: &(impl Fn() + ?Sized),
) {
    let started = Instant::now();
    let mut answers: Vec<(Folder, ReadDirOutcome)> = Vec::with_capacity(walk.len());
    for (folder, path) in walk {
        post(
            tx,
            poke,
            WarmupProgress::Asking {
                generation,
                folder: *folder,
            },
        );
        // `tccd` holds this call until a person answers; no timeout is possible.
        let outcome = (probe.read_dir)(path);
        answers.push((*folder, outcome));
        post(
            tx,
            poke,
            WarmupProgress::Answered {
                generation,
                folder: *folder,
                outcome,
            },
        );
    }
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// Only the app-data item reads `ENOENT` as permission; `EPERM` and `EACCES` fold the same
    /// for every item.
    #[test]
    fn a_missing_path_is_permission_for_the_app_data_item_alone() {
        const EACCES: i32 = 13;
        const ENOENT: i32 = 2;
        assert_eq!(
            fold_read_dir(Folder::AppData, ReadDirOutcome::Failed(ENOENT)),
            WarmupRow::Allowed,
            "being told another app's container is absent means the look was allowed"
        );
        for folder in [Folder::Documents, Folder::Desktop, Folder::Downloads] {
            assert_eq!(
                fold_read_dir(folder, ReadDirOutcome::Failed(ENOENT)),
                WarmupRow::Error,
                "{folder:?}: a missing home folder is not a consent verdict"
            );
        }
        for folder in Folder::ALL {
            assert_eq!(
                fold_read_dir(*folder, ReadDirOutcome::Failed(consent::ERRNO_EPERM)),
                WarmupRow::Denied,
                "{folder:?}"
            );
            assert_eq!(
                fold_read_dir(*folder, ReadDirOutcome::Failed(EACCES)),
                WarmupRow::Error,
                "{folder:?}"
            );
            assert_eq!(
                fold_read_dir(*folder, ReadDirOutcome::Listed),
                WarmupRow::Allowed,
                "{folder:?}"
            );
            assert_eq!(
                fold_read_dir(*folder, ReadDirOutcome::Failed(0)),
                WarmupRow::Error,
                "{folder:?}"
            );
            assert_eq!(
                fold_read_dir(*folder, ReadDirOutcome::Refused),
                WarmupRow::Unknown,
                "{folder:?}: a probe that did not look claims nothing"
            );
        }
    }

    #[test]
    fn every_row_state_has_its_own_word() {
        let words: Vec<&str> = [
            WarmupRow::Unknown,
            WarmupRow::Asking,
            WarmupRow::Allowed,
            WarmupRow::Denied,
            WarmupRow::Error,
        ]
        .into_iter()
        .map(WarmupRow::as_str)
        .collect();
        assert_eq!(
            words,
            vec!["unknown", "asking", "allowed", "denied", "error"]
        );
        assert_eq!(WarmupRow::default(), WarmupRow::Unknown);
    }

    #[test]
    fn a_headless_instance_gets_the_arm_that_cannot_reach_the_filesystem() {
        assert!(!WarmupState::new(true).probe_is_live());
        assert!(WarmupState::new(false).probe_is_live());
        assert!(!WarmupState::default().probe_is_live());
        assert!(!WarmupState::inert().probe_is_live());
        assert_eq!(inert_read_dir(Path::new("/")), ReadDirOutcome::Refused);
    }

    #[test]
    fn the_live_arm_refuses_from_a_binary_outside_a_bundle() {
        // A test binary is not inside a `.app`, so the guard answers before any listing.
        assert_eq!(live_read_dir(Path::new("/")), ReadDirOutcome::Refused);
    }

    #[test]
    fn the_walk_list_drops_repeats_and_keeps_configuration_order() {
        let configured = vec![
            (Folder::Downloads, PathBuf::from("/a")),
            (Folder::Documents, PathBuf::from("/b")),
            (Folder::Downloads, PathBuf::from("/c")),
        ];
        let walk = dedup_folders(&configured);
        assert_eq!(
            walk.iter().map(|(f, _)| *f).collect::<Vec<_>>(),
            vec![Folder::Downloads, Folder::Documents],
            "a folder named twice raises the same dialog twice for no information"
        );
        assert_eq!(walk[0].1, PathBuf::from("/a"));
        assert_eq!(
            folders_to_unknown_rows(&configured),
            vec![
                (Folder::Downloads, WarmupRow::Unknown),
                (Folder::Documents, WarmupRow::Unknown)
            ]
        );
        // A pass's `2n + 1` messages fit the queue.
        assert!(2 * Folder::ALL.len() < RESULT_QUEUE_CAP);
    }

    fn live_state(generation: u64, hold_cap: Duration) -> WarmupState {
        let mut state = WarmupState::inert();
        state.rows = vec![
            (Folder::Documents, WarmupRow::Unknown),
            (Folder::Desktop, WarmupRow::Unknown),
        ];
        state.generation = generation;
        state.live = Some(LivePass {
            generation,
            started_at: Instant::now(),
            hold_cap,
        });
        state
    }

    #[test]
    fn a_message_from_a_retired_pass_cannot_rewrite_a_row() {
        let mut state = live_state(7, Duration::from_secs(120));
        assert!(
            !state.apply(WarmupProgress::Answered {
                generation: 6,
                folder: Folder::Documents,
                outcome: ReadDirOutcome::Failed(consent::ERRNO_EPERM),
            }),
            "a stale generation is dropped"
        );
        assert_eq!(state.rows()[0].1, WarmupRow::Unknown);
        assert!(state.apply(WarmupProgress::Answered {
            generation: 7,
            folder: Folder::Documents,
            outcome: ReadDirOutcome::Failed(consent::ERRNO_EPERM),
        }));
        assert_eq!(state.rows()[0].1, WarmupRow::Denied);
    }

    #[test]
    fn a_pass_that_ends_without_an_answer_clears_asking_to_unknown() {
        let mut state = live_state(1, Duration::from_secs(120));
        assert!(state.apply(WarmupProgress::Asking {
            generation: 1,
            folder: Folder::Documents,
        }));
        assert_eq!(state.rows()[0].1, WarmupRow::Asking);
        state.apply(WarmupProgress::Answered {
            generation: 1,
            folder: Folder::Desktop,
            outcome: ReadDirOutcome::Listed,
        });
        state.end_pass();
        assert_eq!(
            state.rows()[0].1,
            WarmupRow::Unknown,
            "nobody ever learned the answer, and that is not a denial"
        );
        assert_eq!(state.rows()[1].1, WarmupRow::Allowed);
        assert!(!state.is_live());
    }

    #[test]
    fn the_final_message_repairs_a_dropped_one() {
        let mut state = live_state(3, Duration::from_secs(120));
        state.apply(WarmupProgress::Asking {
            generation: 3,
            folder: Folder::Documents,
        });
        // Its `Answered` was dropped; `Finished` carries the whole pass.
        assert!(state.apply(WarmupProgress::Finished {
            generation: 3,
            elapsed_ms: 42,
            answers: vec![
                (Folder::Documents, ReadDirOutcome::Listed),
                (
                    Folder::Desktop,
                    ReadDirOutcome::Failed(consent::ERRNO_EPERM)
                ),
            ],
        }));
        assert_eq!(state.rows()[0].1, WarmupRow::Allowed);
        assert_eq!(state.rows()[1].1, WarmupRow::Denied);
        assert_eq!(state.last_pass_ms(), Some(42));
        assert!(!state.is_live(), "the pass ended, so the hold ends with it");
    }

    #[test]
    fn the_hold_expires_at_the_cap_and_cannot_pin_a_build() {
        let started = Instant::now();
        let cap = Duration::from_millis(120_000);
        assert!(hold_active(started, cap, started));
        assert!(hold_active(
            started,
            cap,
            started + cap - Duration::from_millis(1)
        ));
        assert!(!hold_active(started, cap, started + cap), "at the cap");
        assert!(
            !hold_active(started, cap, started + cap + Duration::from_secs(3600)),
            "an hour later a build is still not pinned"
        );
        assert!(
            !hold_active(started, Duration::ZERO, started),
            "a zero cap never holds"
        );
        // A clock that went backwards holds rather than panics.
        assert!(hold_active(started, cap, started - Duration::from_secs(5)));
    }

    #[test]
    fn a_live_pass_holds_only_the_automatic_apply_and_only_to_the_cap() {
        let cap = Duration::from_millis(120_000);
        let mut state = live_state(1, cap);
        let started = state.live.expect("live").started_at;
        assert!(state.holds_automatic_apply(started));
        assert!(!state.holds_automatic_apply(started + cap));
        assert!(!state.holds_automatic_apply(started + cap + Duration::from_secs(600)));
        // A pass that ended holds nothing.
        state.end_pass();
        assert!(!state.holds_automatic_apply(started));
    }

    #[test]
    fn a_successor_starts_with_no_rows_no_hold_and_no_claim() {
        // An in-place apply builds a fresh process, a fresh `App` and so a fresh
        // `WarmupState`. Nothing is carried over — including any claim about a
        // modal that may still be on the screen (§3.5, §7 S12). The other half
        // of the successor rule, the empty Full Disk Access probe cache, is
        // `consent_state::ConsentState`'s and is asserted there.
        let successor = WarmupState::default();
        assert!(!successor.is_live());
        assert!(successor.rows().is_empty());
        assert_eq!(successor.last_pass_ms(), None);
        assert!(!successor.holds_automatic_apply(Instant::now()));
        assert_eq!(successor.generation, 0);
    }

    /// Holds [`parking_read_dir`] parked until the test releases it, as a `tccd` modal would.
    static PARK: AtomicBool = AtomicBool::new(false);

    fn parking_read_dir(_path: &Path) -> ReadDirOutcome {
        while PARK.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
        ReadDirOutcome::Refused
    }

    #[test]
    fn a_second_gesture_while_a_worker_is_live_is_a_no_op() {
        PARK.store(true, Ordering::Release);
        let mut state = WarmupState::inert();
        state.probe = WarmupProbe {
            read_dir: parking_read_dir,
            live: false,
        };
        let folders = vec![(Folder::Documents, PathBuf::from("/nonexistent-warm-up"))];
        let cap = Duration::from_millis(120_000);
        assert_eq!(
            state.start(&folders, cap, || {}),
            StartOutcome::Started,
            "the gesture starts one worker"
        );
        let generation = state.generation;
        assert_eq!(
            state.start(&folders, cap, || {}),
            StartOutcome::AlreadyLive,
            "a second gesture must not stack a second system modal"
        );
        assert_eq!(
            state.generation, generation,
            "no second pass, so no second worker"
        );
        assert!(state.is_live());
        PARK.store(false, Ordering::Release);
    }

    #[test]
    fn a_pass_with_no_resolvable_folder_starts_nothing() {
        let mut state = WarmupState::inert();
        assert_eq!(
            state.start(&[], Duration::from_millis(120_000), || {}),
            StartOutcome::NoFolders
        );
        assert!(!state.is_live());
        assert!(!state.holds_automatic_apply(Instant::now()));
    }

    #[test]
    fn an_inert_pass_completes_pokes_and_claims_nothing() {
        let mut state = WarmupState::inert();
        let pokes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&pokes);
        let folders = vec![
            (Folder::Documents, PathBuf::from("/nonexistent-warm-up-a")),
            (Folder::Desktop, PathBuf::from("/nonexistent-warm-up-b")),
        ];
        assert_eq!(
            state.start(&folders, Duration::from_millis(120_000), move || {
                counter.fetch_add(1, Ordering::Release);
            }),
            StartOutcome::Started
        );
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while state.is_live() && Instant::now() < deadline {
            state.drain();
            std::thread::yield_now();
        }
        assert!(!state.is_live(), "the pass ended");
        assert!(state.last_pass_ms().is_some());
        assert_eq!(
            state.rows(),
            &[
                (Folder::Documents, WarmupRow::Unknown),
                (Folder::Desktop, WarmupRow::Unknown)
            ],
            "the inert arm asked nothing, so it claims nothing"
        );
        assert!(
            !state.holds_automatic_apply(Instant::now()),
            "a finished pass holds no apply"
        );
        assert!(
            pokes.load(Ordering::Acquire) >= 1,
            "the worker must poke the event loop"
        );
        // The next gesture is free again.
        assert_eq!(
            state.start(&folders, Duration::from_millis(120_000), || {}),
            StartOutcome::Started
        );
    }
}
