// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The warm-up: one deliberate, owner-initiated pre-prompt
//! (`docs/DESIGN-macos-tcc-prompts-2026-08-30.md` §3.5).
//!
//! # What this is
//!
//! On an explicit owner gesture — *Ask for folder access now*, on the Security
//! panel and nowhere else — a detached worker lists each configured folder **in
//! sequence** and records what the filesystem said: `Ok`, `EPERM(1)`, or some
//! other errno. That is all it does. aterm renders nothing, asks nothing, and
//! answers nothing: it performs an ordinary file access at a moment the owner
//! chose, and macOS asks the question it was always going to ask. The whole
//! point is to move the interruption out of an agent's blocked syscall at 3am
//! and into a moment a human is already looking at the screen.
//!
//! Sequential, not concurrent: three simultaneous system modals stack badly,
//! and the organic concurrent case is unmeasured (§1.4 / §7 S11).
//!
//! # Why nothing here may ever be joined
//!
//! **No timeout is possible.** `tccd` holds the calling syscall until a human
//! answers the dialog — there is no deadline, no `EINTR`, no cancel. A worker
//! parked in [`std::fs::read_dir`] can stay parked for the life of the process.
//! So the worker is spawned DETACHED and its `JoinHandle` is dropped on the
//! spot: this module keeps no handle, which makes "join it on the event loop"
//! structurally impossible rather than merely discouraged. (The live incident
//! this rule comes from is on this repo: a `Drop` that joined a worker froze
//! the whole UI — `crates/aterm-gui/src/trail_audio.rs`.)
//!
//! Results come back over a BOUNDED channel the producer `try_send`s into and
//! DROPS on `Full` (the `notify.rs` queue is the precedent), plus a payload-free
//! poke of the event loop. A dropped poke is survivable and a dropped message is
//! survivable: the channel's `Disconnected` edge is the authoritative
//! end-of-pass signal, and it cannot be lost.
//!
//! # No protected-folder path literal lives here
//!
//! The folders arrive as already-resolved `(Folder, PathBuf)` pairs from
//! `aterm_containment::consent::folder_paths`. This module writes none of its
//! own — that is the B13 rule (`tools/grep_guard.sh`), and
//! `tests::the_module_contains_no_protected_path_literal` asserts it about
//! this very file.
//!
//! # Not reachable from a control verb
//!
//! Default `warmup = "on-request"`; there is no `"first-launch"` value, no
//! environment variable, no `VERBS` row, and no dispatch arm. A consent-raising
//! action reachable from inside a session would be a consent surface an agent
//! controls, which is the same rule that governs `tccutil reset` (§3.7).
//! `tests::no_control_dispatch_arm_can_reach_the_warm_up` is the fence.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::time::{Duration, Instant};

use aterm_containment::consent::{self, Folder};

/// Bound on the warm-up result queue.
///
/// A pass posts at most `2n + 1` messages for `n` folders and `n` is at most
/// [`Folder::ALL`]`.len()` (the walk list is de-duplicated), so this cap is
/// never reached in practice — it exists for the same structural reason
/// `notify.rs`'s does: a producer that cannot block the thread it runs on must
/// have somewhere bounded to put its output. The overflow policy is DROP, never
/// coalesce; a dropped message is repaired by the pass's final message, and a
/// dropped final message is repaired by the channel's `Disconnected` edge.
const RESULT_QUEUE_CAP: usize = 16;

// ---------------------------------------------------------------------------
// The pure fold: what one directory listing means
// ---------------------------------------------------------------------------

/// What one folder's listing attempt returned.
///
/// `Failed` carries the RAW errno rather than a classification, so the fold
/// below is the single place that decides what an errno means and the table
/// test can drive it directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadDirOutcome {
    /// The directory listed. Either it had an entry we could read, or it was
    /// empty — both mean the access was permitted.
    Listed,
    /// The listing failed with this raw errno.
    Failed(i32),
    /// The probe was deliberately NOT performed: a headless instance, a unit
    /// test, or an executable that does not resolve inside a `.app`. This is
    /// not a denial and must never be rendered as one.
    Refused,
}

/// One folder's state on the Security panel.
///
/// `Unknown` is a real answer, not a failure — it is what a folder reads before
/// anything asked, and what every folder reads again in a successor process.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum WarmupRow {
    /// Nothing has been asked about this folder in this process.
    #[default]
    Unknown,
    /// The worker is inside the listing syscall for this folder RIGHT NOW. It
    /// may be parked behind a system dialog, and there is no timeout: this row
    /// can stay here indefinitely, which is the honest thing for it to do.
    Asking,
    /// The listing succeeded from this process.
    Allowed,
    /// `EPERM(1)` — the TCC refusal. macOS is not asking again for this folder.
    Denied,
    /// Some other errno. Not a consent verdict; the folder may not exist, or
    /// the volume may be gone.
    Error,
}

impl WarmupRow {
    /// The report spelling, which is also the panel spelling.
    ///
    /// PENDING CONSUMER: the Security panel (§3.4) and the `privacy` verb's
    /// folder rows are the callers, and they land in the change after this one.
    #[allow(dead_code)]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Asking => "asking",
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::Error => "error",
        }
    }
}

/// THE FOLD. `Ok` / `EPERM` / anything else → a row state.
///
/// Pure and total, and deliberately the only place the mapping exists: a
/// classification that lives inside a worker thread is a classification no test
/// can reach.
///
/// `EPERM` and `EACCES` are NOT interchangeable here (design §1.5): a TCC
/// refusal is `EPERM`, and an ordinary Unix permission problem is `EACCES`.
/// Folding `EACCES` to `Denied` would report a `chmod` as a privacy denial.
pub(crate) const fn fold_read_dir(outcome: ReadDirOutcome) -> WarmupRow {
    match outcome {
        ReadDirOutcome::Listed => WarmupRow::Allowed,
        ReadDirOutcome::Failed(consent::ERRNO_EPERM) => WarmupRow::Denied,
        ReadDirOutcome::Failed(_) => WarmupRow::Error,
        ReadDirOutcome::Refused => WarmupRow::Unknown,
    }
}

// ---------------------------------------------------------------------------
// THE FENCE: the injected listing probe
// ---------------------------------------------------------------------------

/// The one OS question this module asks, behind the injection every consent
/// surface uses (`ConsentProbes`, `lock_modifiers`, `user_input_recent`).
///
/// * [`WarmupProbe::live`] — a WINDOWED instance: a real directory listing.
/// * [`WarmupProbe::inert`] — a headless instance and every unit test: answers
///   [`ReadDirOutcome::Refused`] without a syscall.
///
/// The live arm ALSO re-checks the in-bundle guard on every call, so the module
/// defends itself no matter who constructs it. The two gates are deliberately
/// redundant, exactly as `aterm_containment::consent`'s are: on 2026-08-17 a
/// test binary's first `WindowServer` touch made `tccd` `readdir` a
/// 1.1-million-entry `target/debug/deps` until `WindowServer` was killed.
#[derive(Clone, Copy)]
pub(crate) struct WarmupProbe {
    /// Lists one already-resolved directory. May park indefinitely.
    read_dir: fn(&Path) -> ReadDirOutcome,
    /// Whether this is the live arm. Reported, never inferred: a probe that was
    /// never consulted is not a probe that answered no.
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

    /// The headless / unit-test arm: no syscall, and `unknown` said out loud.
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
    #[allow(dead_code)]
    pub(crate) const fn is_live(self) -> bool {
        self.live
    }
}

/// The inert arm.
fn inert_read_dir(_path: &Path) -> ReadDirOutcome {
    ReadDirOutcome::Refused
}

/// The live arm: ONE directory listing of an already-resolved path.
///
/// Re-applies the in-bundle guard first (design §3.3 guardrail 1): a binary
/// under `target/debug/deps` never reaches the syscall, so a unit test that
/// somehow acquired the live arm still cannot park a system modal in front of
/// the test runner.
///
/// Taking the first entry is deliberate: `opendir` succeeding is not proof the
/// enumeration is permitted, and the first `readdir` is where the refusal
/// actually surfaces. An empty directory reads as `Listed`, which is correct —
/// nothing refused.
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

/// Off macOS there is no TCC, so there is nothing to warm up and no reason to
/// walk the user's folders. The live arm refuses like the inert one, and says
/// so with the same word.
#[cfg(not(target_os = "macos"))]
fn live_read_dir(_path: &Path) -> ReadDirOutcome {
    ReadDirOutcome::Refused
}

// ---------------------------------------------------------------------------
// Worker → main-thread messages
// ---------------------------------------------------------------------------

/// One message from a warm-up worker.
///
/// Every variant carries the pass `generation` so a message from a worker this
/// instance has already retired cannot rewrite a newer pass's rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WarmupProgress {
    /// The worker is about to enter the listing syscall for this folder. Posted
    /// BEFORE the call, because the call is exactly what may never return.
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
    /// A worker is already walking. The gesture is a DELIBERATE no-op: a second
    /// worker would stack a second system modal behind the first, which is the
    /// exact thing walking the folders in sequence exists to avoid.
    AlreadyLive,
    /// `[privacy] enabled = false`, `warmup = "never"`, or an instance with no
    /// Security panel to have gestured from. Nothing was started, nothing was
    /// asked.
    Refused,
    /// No configured folder name resolved to a path — every name was unknown,
    /// or `$HOME` is unset.
    NoFolders,
    /// The OS refused the thread. Nothing was started.
    SpawnFailed,
}

// ---------------------------------------------------------------------------
// The instance's warm-up state
// ---------------------------------------------------------------------------

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

/// The instance's warm-up state: the rows, the live pass, and the receiving end
/// of the bounded result queue.
///
/// Instance-owned, with NO process-global anywhere in this module
/// (`tests::the_module_owns_no_process_global_and_reads_no_environment_knob` is
/// the fence). That is what
/// makes §3.5's successor rule structural rather than a cleanup step: an
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
    /// Wall time of the last COMPLETED pass.
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

    /// Whether this instance's probe can reach the filesystem. The panel reads
    /// it to say "this instance is not looking" rather than "nothing is
    /// granted" — PENDING CONSUMER, like [`WarmupRow::as_str`].
    #[allow(dead_code)]
    pub(crate) const fn probe_is_live(&self) -> bool {
        self.probe.is_live()
    }

    /// Start one pass over `folders`, which are ALREADY-RESOLVED paths handed
    /// in as data (this module resolves nothing and holds no path literal).
    ///
    /// `hold_cap` is `[privacy] warmup_hold_ms`, already clamped by the config
    /// resolver. `poke` is called after every message is queued; it is the
    /// event-loop wake, abstracted so the posting discipline is unit-testable
    /// without an event loop (the `PkgProgressTailer` precedent).
    ///
    /// A second call while a pass is live returns [`StartOutcome::AlreadyLive`]
    /// and changes NOTHING — not the rows, not the generation, not the hold.
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
        // DETACHED ON PURPOSE: the handle is dropped here and never stored, so
        // no `Drop`, no shutdown path and no future edit can join a thread that
        // may be parked in `tccd` with no timeout.
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

    /// Fold every queued message into the rows. Returns how many were applied.
    ///
    /// Cheap and total: `try_recv` until the queue is empty. `Disconnected` is
    /// the authoritative end-of-pass edge — the worker dropped its sender by
    /// exiting — and it is the one signal that cannot be dropped by a full
    /// queue, so a pass always ends even if every message it sent was lost.
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

    /// Fold ONE message. Returns whether it was for the live pass.
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
            } => self.set_row(folder, fold_read_dir(outcome)),
            WarmupProgress::Finished {
                elapsed_ms,
                answers,
                ..
            } => {
                for (folder, outcome) in answers {
                    self.set_row(folder, fold_read_dir(outcome));
                }
                self.last_pass_ms = Some(elapsed_ms);
                self.end_pass();
            }
        }
        true
    }

    /// Retire the live pass. Any row still `asking` becomes `unknown`: the
    /// worker is gone and nobody ever learned the answer, which is a different
    /// fact from a denial.
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

    /// Arm a hold with no worker behind it, so the event loop's automatic-apply
    /// gate can be driven from a unit test that must not spawn anything. Test
    /// seam only: production arms the hold through [`Self::start`], and only
    /// alongside a real worker.
    #[cfg(test)]
    pub(crate) fn arm_hold_for_test(&mut self, started_at: Instant, hold_cap: Duration) {
        self.generation = self.generation.wrapping_add(1);
        self.live = Some(LivePass {
            generation: self.generation,
            started_at,
            hold_cap,
        });
    }

    /// THE IN-PLACE-APPLY HOLD (design §3.5).
    ///
    /// True while a warm-up worker is live AND the hold has not reached its
    /// cap. The automatic apply already backs off while the user is interacting
    /// (`App::update_apply_hands_off_keys`); this is the same shape and the same
    /// bound — an owner-initiated, seconds-long interaction, hard-capped, and
    /// consulted ONLY on the automatic lanes. A manual `aterm ctl update apply`
    /// is never held.
    ///
    /// It is explicitly NOT "defer while agents are live", which §3.9 refuses:
    /// that trades a shipped invariant (shells survive an update) for a
    /// permissions nicety and lets one long-lived session pin an old build.
    ///
    /// The cap is also what makes a MISSED drain harmless: a pass whose messages
    /// were never folded still stops holding at the cap, so this can never pin a
    /// build even if the event loop stopped listening.
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

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Whether a hold armed at `started_at` with cap `hold_cap` is still active at
/// `now`. Saturating, so a clock that went backwards holds rather than panics,
/// and a zero cap never holds at all.
pub(crate) fn hold_active(started_at: Instant, hold_cap: Duration, now: Instant) -> bool {
    now.saturating_duration_since(started_at) < hold_cap
}

/// The walk list: the configured pairs with repeats removed, order preserved.
///
/// A config may name the same folder twice; walking it twice would raise the
/// same dialog twice for no information. This also pins the pass's message
/// count at `2n + 1` with `n <= Folder::ALL.len()`, which is what makes
/// [`RESULT_QUEUE_CAP`] unreachable in practice.
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

// ---------------------------------------------------------------------------
// The worker
// ---------------------------------------------------------------------------

/// Queue one message and poke the event loop.
///
/// `try_send` and DROP on `Full` — never block, never coalesce. The poke is
/// issued either way: the main thread's drain is what discovers both the
/// message and, at the end, the disconnect.
fn post(tx: &SyncSender<WarmupProgress>, poke: &(impl Fn() + ?Sized), message: WarmupProgress) {
    match tx.try_send(message) {
        Ok(()) | Err(TrySendError::Disconnected(_)) => {}
        Err(TrySendError::Full(dropped)) => {
            aterm_log::debug!("consent warm-up dropped a full result queue's message: {dropped:?}");
        }
    }
    poke();
}

/// THE PASS. Walks the folders in sequence; each listing may park indefinitely
/// behind a system dialog, which is exactly why this runs on a detached thread
/// that nothing joins.
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
        // THE PARK. `tccd` holds this call until a human answers. No timeout is
        // possible, and none is faked.
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

    /// The module's SHIPPING source: everything before the first `#[cfg(test)]`
    /// attribute, which is this module's own. Mirrors `tools/grep_guard.sh`'s
    /// `np_strip`, and it is what lets the scans below name the very patterns
    /// they are forbidding.
    fn shipping_source() -> &'static str {
        const SOURCE: &str = include_str!("consent_warmup.rs");
        let marker = "#[cfg(test)]";
        let shipping = SOURCE
            .split(marker)
            .next()
            .expect("split always yields a first part");
        assert!(
            shipping.len() < SOURCE.len(),
            "the test module must be excluded from the scan"
        );
        shipping
    }

    // -----------------------------------------------------------------------
    // The pure fold
    // -----------------------------------------------------------------------

    #[test]
    fn the_fold_maps_ok_eperm_and_everything_else() {
        // EACCES is the one that must NOT read as a privacy denial: a TCC
        // refusal is EPERM (design §1.5), an ordinary Unix permission problem
        // is EACCES, and folding them together would report a `chmod` as a
        // consent verdict.
        const EACCES: i32 = 13;
        const ENOENT: i32 = 2;
        let table = [
            (ReadDirOutcome::Listed, WarmupRow::Allowed),
            (
                ReadDirOutcome::Failed(consent::ERRNO_EPERM),
                WarmupRow::Denied,
            ),
            (ReadDirOutcome::Failed(EACCES), WarmupRow::Error),
            (ReadDirOutcome::Failed(ENOENT), WarmupRow::Error),
            (ReadDirOutcome::Failed(0), WarmupRow::Error),
            (ReadDirOutcome::Refused, WarmupRow::Unknown),
        ];
        for (outcome, expected) in table {
            assert_eq!(fold_read_dir(outcome), expected, "folding {outcome:?}");
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

    // -----------------------------------------------------------------------
    // The fence
    // -----------------------------------------------------------------------

    #[test]
    fn the_module_contains_no_protected_path_literal() {
        let shipping = shipping_source();
        // The same set `tools/grep_guard.sh` B13a/B13b forbid in aterm-gui and
        // aterm-cli. Paths reach this module as already-resolved `PathBuf`s.
        let literals = [
            "~/Documents",
            "~/Desktop",
            "~/Downloads",
            "~/Pictures",
            "~/Movies",
            "~/Music",
            "/Volumes",
            "Library/Containers",
            "Library/CloudStorage",
            "CloudStorage",
            "Containers",
        ];
        for needle in literals {
            assert!(
                !shipping.contains(needle),
                "the warm-up module must hold no protected-folder path literal, found {needle:?}"
            );
        }
        let joins = [
            "join(\"Documents\")",
            "join(\"Desktop\")",
            "join(\"Downloads\")",
            "join(\"Volumes\")",
            "join(\"Containers\")",
            "join(\"CloudStorage\")",
        ];
        for needle in joins {
            assert!(
                !shipping.contains(needle),
                "a resolved-at-$HOME protected path is still a path literal, found {needle:?}"
            );
        }
    }

    #[test]
    fn the_module_owns_no_process_global_and_reads_no_environment_knob() {
        let shipping = shipping_source();
        for line in shipping.lines() {
            let trimmed = line.trim_start();
            assert!(
                !trimmed.starts_with("static ")
                    && !trimmed.starts_with("pub static ")
                    && !trimmed.starts_with("pub(crate) static "),
                "a process-global would survive an in-place apply; this state is \
                 instance-owned so a successor re-probes: {line}"
            );
        }
        assert!(!shipping.contains("OnceLock"));
        assert!(!shipping.contains("thread_local!"));
        // No environment variable may reach the warm-up: `ATERM_*` is stripped
        // from children anyway, and a knob a program inside a session could flip
        // would be a consent surface an agent controls (design §4).
        assert!(!shipping.contains("env::var"));
    }

    #[test]
    fn no_control_dispatch_arm_can_reach_the_warm_up() {
        // The consent-RAISING entry points. Reading the rows is harmless and is
        // deliberately not on this list; starting a pass is what puts a system
        // modal on the owner's screen, and a program inside a session must not
        // be able to do that.
        const ENTRY_POINTS: &[&str] = &[
            "begin_consent_warmup",
            "WarmupState::start",
            "consent_warmup::WarmupProbe",
        ];
        let sources: &[(&str, &str)] = &[
            ("control.rs", include_str!("control.rs")),
            ("control_auth.rs", include_str!("control_auth.rs")),
            ("control_auth_unix.rs", include_str!("control_auth_unix.rs")),
            ("control_auth_win.rs", include_str!("control_auth_win.rs")),
            ("control_host.rs", include_str!("control_host.rs")),
            ("control_input.rs", include_str!("control_input.rs")),
            ("control_media.rs", include_str!("control_media.rs")),
            ("control_privacy.rs", include_str!("control_privacy.rs")),
            ("control_query.rs", include_str!("control_query.rs")),
            ("control_session.rs", include_str!("control_session.rs")),
            (
                "control_connection_conformance.rs",
                include_str!("control_connection_conformance.rs"),
            ),
            (
                "control_redraw_conformance.rs",
                include_str!("control_redraw_conformance.rs"),
            ),
        ];
        for (name, source) in sources {
            for entry in ENTRY_POINTS {
                assert!(
                    !source.contains(entry),
                    "{name} reaches the warm-up entry point `{entry}`: a consent-raising \
                     action reachable from inside a session is a consent surface an agent \
                     controls (design §3.5)"
                );
            }
        }
        // …and the entry point really is spelled that way, so a rename cannot
        // silently defang the scan above.
        assert!(
            include_str!("lib.rs").contains("fn begin_consent_warmup"),
            "the gesture entry point moved; update ENTRY_POINTS"
        );
    }

    // -----------------------------------------------------------------------
    // The probe arms
    // -----------------------------------------------------------------------

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
        // This test binary lives under `target/debug/deps`, so the in-bundle
        // fence answers before any listing happens. `/` is not protected and is
        // not touched either way — the point is that the guard returns first.
        assert_eq!(live_read_dir(Path::new("/")), ReadDirOutcome::Refused);
    }

    // -----------------------------------------------------------------------
    // Walk-list shaping
    // -----------------------------------------------------------------------

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
        // At most `Folder::ALL.len()` folders ⇒ at most `2n + 1` messages, which
        // is what keeps the bounded queue's drop path unreachable in practice.
        assert!(2 * Folder::ALL.len() < RESULT_QUEUE_CAP);
    }

    // -----------------------------------------------------------------------
    // Folding messages
    // -----------------------------------------------------------------------

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
        // Its `Answered` never arrived — the queue was full. `Finished` carries
        // the whole pass, so the row still lands.
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

    // -----------------------------------------------------------------------
    // The in-place-apply hold
    // -----------------------------------------------------------------------

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
        // Backwards clock: saturating, so it holds rather than panics, and the
        // cap still ends it.
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
        // …and a pass that ended holds nothing at all.
        state.end_pass();
        assert!(!state.holds_automatic_apply(started));
    }

    #[test]
    fn a_manual_apply_is_never_held() {
        use crate::native_updater_service::ApplyMode;
        assert!(ApplyMode::Automatic.is_automatic());
        assert!(ApplyMode::AutomaticPastGrace.is_automatic());
        assert!(
            !ApplyMode::Immediate.is_automatic(),
            "an explicit `aterm ctl update apply` is the user asking for the freeze"
        );
        assert!(!ApplyMode::CleanQuit.is_automatic());
        // The hold is consulted from exactly one place — the automatic-apply
        // freeze gate — and that gate is reached only under `is_automatic()`.
        let lib = include_str!("lib.rs");
        assert!(
            lib.contains("self.consent_warmup.holds_automatic_apply(now)"),
            "the hold must be wired into the automatic-apply freeze gate"
        );
        let apply_lane = include_str!("app_native.rs");
        let call_sites: Vec<&str> = apply_lane
            .lines()
            .filter(|line| line.contains("update_apply_hands_off_keys"))
            .collect();
        assert!(
            !call_sites.is_empty(),
            "the freeze gate lost its only caller"
        );
        for line in call_sites {
            assert!(
                line.contains("is_automatic()"),
                "the freeze gate (and so the warm-up hold) may only be consulted on an \
                 automatic lane: {line}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // The successor
    // -----------------------------------------------------------------------

    #[test]
    fn a_successor_starts_with_no_rows_no_hold_and_no_claim() {
        // An in-place apply builds a fresh process, a fresh `App` and so a fresh
        // `WarmupState`. Nothing is carried over — including any claim about a
        // modal that may still be on the screen (§3.5, §7 S12). The other half
        // of the successor rule, the empty Full Disk Access probe cache, is
        // `control_privacy::ConsentState`'s and is asserted there.
        let successor = WarmupState::default();
        assert!(!successor.is_live());
        assert!(successor.rows().is_empty());
        assert_eq!(successor.last_pass_ms(), None);
        assert!(!successor.holds_automatic_apply(Instant::now()));
        assert_eq!(successor.generation, 0);
    }

    // -----------------------------------------------------------------------
    // The worker
    // -----------------------------------------------------------------------

    /// A listing arm that parks until the test releases it — the shape a real
    /// `tccd` modal has, without a modal.
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
        let deadline = Instant::now() + Duration::from_secs(10);
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
        // And the next gesture is free again.
        assert_eq!(
            state.start(&folders, Duration::from_millis(120_000), || {}),
            StartOutcome::Started
        );
    }
}
