// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The vendor head watch (design §1.6): the window's package thread asks each vendor-direct
//! program's head about every minute on the wall clock, and once shortly after a wake, with
//! the ETag it last saw, and names the programs whose head is newer than the build
//! installed. The head is only a hint: the `update <program>` pass it triggers re-verifies
//! everything.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

pub use super::resolve::VendorGetFn;
use super::resolve::{self, Head};
use super::table::{VENDORS, VendorSpec};
use super::version::{Version, is_vendor_build};
use crate::flow::{VendorFetchError, VendorGet};
use crate::store::Layout;

/// A head GET that independent vendor checks may call on separate threads.
pub type ConcurrentVendorGetFn<'a> =
    dyn Fn(&str, &str, u64, Option<&str>) -> Result<VendorGet, VendorFetchError> + Send + Sync + 'a;

/// How often each head is asked across the whole store, regardless of window count.
pub const CADENCE: Duration = Duration::from_secs(60);
/// How soon a head that was not reached is asked again — once; then at [`CADENCE`].
pub const UNREACHABLE_RETRY: Duration = Duration::from_secs(60);
/// How long after a detected wake the heads are asked: the network's time to come back.
pub const AFTER_WAKE: Duration = Duration::from_secs(20);
/// A head already offered is offered again only after this long, so a version the lane
/// refuses costs one pass an hour, not one every check.
pub const REOFFER_AFTER: Duration = Duration::from_secs(60 * 60);
/// One quick retry after a head-watch pass left the installed version behind.
pub const FAILED_PASS_RETRY: Duration = Duration::from_secs(5 * 60);
const MAX_SHORT_RETRIES: u8 = 1;
const BUSY_RETRY: Duration = Duration::from_secs(5);
const SHARED_SCHEMA: u32 = 2;
const MAX_SHARED_BYTES: u64 = 1024;
const DURABLE_CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// The wall clock outrunning the monotonic clock by more than this over one slice is a
/// wake: the monotonic clock stops while the machine sleeps.
pub const WAKE_GAP: Duration = Duration::from_secs(30);

/// One small, store-scoped file per vendor, also serving as its flock inode. The file is
/// only a wake hint: neither its version nor its validator can authorize an install.
/// A corrupt or interrupted write simply causes the next holder to fetch again.
#[derive(Debug, Serialize, Deserialize)]
struct SharedState {
    schema: u32,
    source: String,
    head: Option<Version>,
    etag: Option<String>,
    offered: Option<Version>,
    offered_at: Option<u64>,
    next_check_at: Option<u64>,
    last_fetch_at: Option<u64>,
    last_record_at: Option<u64>,
    unreachable: u32,
    short_retries: u8,
    pending: bool,
    offer_generation: u64,
}

impl SharedState {
    fn empty(spec: &VendorSpec) -> Self {
        Self {
            schema: SHARED_SCHEMA,
            source: spec.head_url.to_string(),
            head: None,
            etag: None,
            offered: None,
            offered_at: None,
            next_check_at: None,
            last_fetch_at: None,
            last_record_at: None,
            unreachable: 0,
            short_retries: 0,
            pending: false,
            offer_generation: 0,
        }
    }

    fn watched(&self) -> Watched {
        Watched {
            etag: self.etag.clone(),
            head: self.head,
            offered: self
                .offered
                .zip(self.offered_at)
                .map(|(v, at)| (v, unix_time(at))),
            next_check_at: self.next_check_at.map(unix_time),
            last_fetch_at: self.last_fetch_at.map(unix_time),
            unreachable: self.unreachable,
            short_retries: self.short_retries,
            pending: self.pending,
            offer_generation: self.offer_generation,
            deferred_result: None,
        }
    }

    fn remember(&mut self, watched: &Watched) {
        self.etag = watched.etag.clone();
        self.head = watched.head;
        self.offered = watched.offered.map(|(v, _)| v);
        self.offered_at = watched.offered.and_then(|(_, at)| unix_seconds(at));
        self.next_check_at = watched.next_check_at.and_then(unix_seconds);
        self.last_fetch_at = watched.last_fetch_at.and_then(unix_seconds);
        self.unreachable = watched.unreachable;
        self.short_retries = watched.short_retries;
        self.pending = watched.pending;
        self.offer_generation = watched.offer_generation;
    }
}

fn unix_time(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn unix_seconds(time: SystemTime) -> Option<u64> {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// A child result waiting for the watch file's lock. The offer identity prevents a
/// delayed result from clearing a newer offer of the same vendor version.
#[derive(Clone, Copy, Debug)]
struct DeferredResult {
    version: Version,
    generation: u64,
    offered_at: u64,
    success: bool,
    reported_at: SystemTime,
}

/// One program's watch state.
#[derive(Debug, Clone, Default)]
struct Watched {
    /// The validator of the head last read in full.
    etag: Option<String>,
    /// The head last read in full.
    head: Option<Version>,
    /// The head last offered, and when.
    offered: Option<(Version, SystemTime)>,
    /// When the head is next asked; `None` is now.
    next_check_at: Option<SystemTime>,
    /// Last actual GET, distinct from a cached-head offer at a retry deadline.
    last_fetch_at: Option<SystemTime>,
    /// Consecutive answers that did not reach the host.
    unreachable: u32,
    /// Short retries already granted for a pass that did not reach this head.
    short_retries: u8,
    /// The offered head has not yet reported a targeted pass result.
    pending: bool,
    /// Incremented for every offer, including another offer of the same version.
    offer_generation: u64,
    /// A result that could not take the shared lock when the child exited.
    deferred_result: Option<DeferredResult>,
}

impl Watched {
    /// Due at `now`; a schedule materially further out than [`CADENCE`] is a wall
    /// clock set back. The margin matters: another process may stamp a minute from
    /// its clock a few milliseconds after our caller sampled `now`.
    fn due(&self, now: SystemTime) -> bool {
        self.next_check_at
            .is_none_or(|at| at <= now || at > now + CADENCE + WAKE_GAP)
    }

    /// Offer `head` unless it is the head last offered, less than [`REOFFER_AFTER`] ago:
    /// `None` when not, else whether it is that head offered again.
    fn offer(&mut self, head: Version, now: SystemTime) -> Option<bool> {
        let again = match self.offered {
            Some((offered, at)) if offered == head => {
                let cooldown = if (self.pending && self.short_retries <= MAX_SHORT_RETRIES)
                    || (!self.pending && self.short_retries == MAX_SHORT_RETRIES)
                {
                    FAILED_PASS_RETRY
                } else {
                    REOFFER_AFTER
                };
                if now.duration_since(at).is_ok_and(|since| since < cooldown) {
                    // A retry deadline between two GET ticks must not be rounded up
                    // to the following minute (the watcher owns user-visible latency).
                    if let Some(deadline) = at.checked_add(cooldown) {
                        self.next_check_at = Some(
                            self.next_check_at
                                .map_or(deadline, |scheduled| scheduled.min(deadline)),
                        );
                    }
                    return None;
                }
                if self.pending {
                    self.short_retries = self.short_retries.saturating_add(1);
                }
                true
            }
            _ => {
                self.short_retries = 0;
                false
            }
        };
        self.offered = Some((head, now));
        self.pending = true;
        self.offer_generation = self.offer_generation.saturating_add(1);
        // A local fallback can make a new offer while the shared file is
        // unavailable. Its old child's deferred result no longer speaks for it.
        self.deferred_result = None;
        Some(again)
    }

    fn result_for_current_offer(&self, success: bool, now: SystemTime) -> Option<DeferredResult> {
        let (version, offered_at) = self.offered?;
        self.pending.then_some(DeferredResult {
            version,
            generation: self.offer_generation,
            offered_at: unix_seconds(offered_at)?,
            success,
            reported_at: now,
        })
    }

    fn matches_result(&self, result: DeferredResult) -> bool {
        self.pending
            && self.offer_generation == result.generation
            && self
                .offered
                .and_then(|(v, at)| unix_seconds(at).map(|s| (v, s)))
                == Some((result.version, result.offered_at))
    }

    fn apply_result(&mut self, result: DeferredResult) {
        if !result.success {
            self.short_retries = self.short_retries.saturating_add(1);
        }
        self.pending = false;
        self.offered = Some((result.version, result.reported_at));
    }
}

/// One bounded, owned vendor check. Its result remains joinable until the package
/// worker harvests it; a completed check is not mistaken for an absent one.
#[derive(Debug)]
struct PendingHead {
    handle: JoinHandle<CompletedHead>,
    /// A machine wake while the GET was in flight moves the *next* check to the
    /// wake grace, rather than letting an older worker snapshot undo that schedule.
    wake_after: Option<SystemTime>,
}

#[derive(Debug)]
struct CompletedHead {
    index: usize,
    watched: Watched,
    outcome: CheckOutcome,
    notes: Vec<String>,
}

/// The installed build, as the watch compares it with a head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Installed {
    /// A vendor build and its record.
    Version(Version),
    /// A legacy index build whose version no pass has read, or a vendor build without a
    /// complete record: older than any head, so the lane decides.
    Unknown,
}

impl Installed {
    fn older_than(self, head: Version) -> bool {
        match self {
            Self::Version(v) => v < head,
            Self::Unknown => true,
        }
    }

    fn words(self) -> String {
        match self {
            Self::Version(v) => format!("installed {v}"),
            Self::Unknown => String::from("installed version unknown"),
        }
    }
}

/// The per-process head watch over the vendor-direct programs.
#[derive(Debug)]
pub struct HeadWatch {
    /// Every vendor-direct program, excluded or not: an exclusion lifted mid-window
    /// resumes the program's watch where it stood ([`Self::set_exclude`]).
    watched: Vec<(&'static VendorSpec, Watched)>,
    /// `[packages].exclude` as the caller last read it.
    exclude: Vec<String>,
    notes: Vec<String>,
    shared: bool,
    shared_prefix: Option<PathBuf>,
    pending: Vec<Option<PendingHead>>,
    completed_tx: mpsc::Sender<usize>,
    completed_rx: mpsc::Receiver<usize>,
    ready_notices: Vec<usize>,
    /// A completed hint whose park was preempted by a bump, full-pass deadline,
    /// or index answer. The next park rechecks the live install before offering it.
    deferred_moved: Vec<&'static str>,
    round_notes: Vec<Option<Vec<String>>>,
    round_active: bool,
    #[cfg(test)]
    fail_next_pending_spawn: bool,
}

impl HeadWatch {
    /// A watch with every head due now, over every vendor-direct program but those
    /// `exclude` (`[packages].exclude`) names.
    #[must_use]
    pub fn new(exclude: &[String]) -> Self {
        let watched: Vec<_> = VENDORS
            .iter()
            .map(|spec| (spec, Watched::default()))
            .collect();
        let (completed_tx, completed_rx) = mpsc::channel();
        let pending = watched.iter().map(|_| None).collect();
        let round_notes = watched.iter().map(|_| None).collect();
        Self {
            watched,
            exclude: exclude.to_vec(),
            notes: Vec::new(),
            shared: false,
            shared_prefix: None,
            pending,
            completed_tx,
            completed_rx,
            ready_notices: Vec::new(),
            deferred_moved: Vec::new(),
            round_notes,
            round_active: false,
            #[cfg(test)]
            fail_next_pending_spawn: false,
        }
    }

    /// `[packages].exclude` as it reads NOW. A long-lived caller (the window's loop) hands
    /// it in before every slice, so an edit is heard without a relaunch: an excluded
    /// program is neither due nor asked, and one taken off the list is asked at its next
    /// due time. The other per-program gates — removed, held by a local pin, dev-linked —
    /// are read off the store at every check ([`Self::check`]).
    pub fn set_exclude(&mut self, exclude: &[String]) {
        if self.exclude != exclude {
            self.exclude = exclude.to_vec();
        }
    }

    fn excludes(exclude: &[String], program: &str) -> bool {
        exclude.iter().any(|p| p == program)
    }

    /// The production watch shares each vendor's check and validator across windows.
    fn shared(exclude: &[String]) -> Self {
        let mut watch = Self::new(exclude);
        watch.shared = true;
        watch
    }

    /// Whether any head the exclusion leaves watched is due at `now` (the wall clock).
    #[must_use]
    pub fn due(&self, now: SystemTime) -> bool {
        !self.deferred_moved.is_empty()
            || self.has_pending()
            || self
                .watched
                .iter()
                .any(|(spec, w)| !Self::excludes(&self.exclude, spec.program) && w.due(now))
    }

    /// Keep a completed offer when a higher-priority package event wins this
    /// park. A signed pass or another process may install it meanwhile, so the
    /// replay checks the live store instead of blindly launching a second pass.
    pub fn defer_moved(&mut self, programs: &[&'static str]) {
        for &program in programs {
            if !Self::excludes(&self.exclude, program)
                && self.watched.iter().any(|(spec, _)| spec.program == program)
                && !self.deferred_moved.contains(&program)
            {
                self.deferred_moved.push(program);
            }
        }
    }

    fn take_deferred_moved(&mut self, layout: &Layout) -> Vec<&'static str> {
        std::mem::take(&mut self.deferred_moved)
            .into_iter()
            .filter(|program| {
                !Self::excludes(&self.exclude, program)
                    && self.watched.iter().any(|(spec, watched)| {
                        spec.program == *program
                            && watched.offered.is_some_and(|(head, _)| {
                                installed(layout, spec)
                                    .is_some_and(|active| active.older_than(head))
                            })
                    })
            })
            .collect()
    }

    /// A check already launched but not yet harvested. Queued completions still
    /// count, so a second check round cannot overtake their state or their notes.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.iter().filter(|slot| slot.is_some()).count()
    }

    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.pending_count() != 0
    }

    /// Park until one vendor check finishes or this ordinary package-loop slice
    /// elapses. The same timed wait still bounds settings and bump responsiveness;
    /// a ready head wakes the worker immediately, even while its peer is stalled.
    pub fn park_for_hint(&mut self, slice: Duration) {
        if !self.deferred_moved.is_empty() {
            return;
        }
        if !self.has_pending() {
            std::thread::sleep(slice);
        } else if self.ready_notices.is_empty()
            && let Ok(index) = self.completed_rx.recv_timeout(slice)
        {
            self.ready_notices.push(index);
        }
    }

    /// One slice of the caller's park, over which the wall clock moved `wall` (`None`:
    /// backwards) and the monotonic clock `mono`. A wake schedules every head
    /// [`AFTER_WAKE`] from `now`; answers whether this slice was one.
    pub fn note_slice(&mut self, now: SystemTime, wall: Option<Duration>, mono: Duration) -> bool {
        let woke = wall.is_some_and(|w| w > mono.saturating_add(WAKE_GAP));
        if woke {
            for (_, w) in &mut self.watched {
                w.next_check_at = Some(now + AFTER_WAKE);
            }
            for pending in self.pending.iter_mut().flatten() {
                pending.wake_after = Some(now + AFTER_WAKE);
            }
        }
        woke
    }

    /// Ask every due head through `get`, conditionally on its last ETag, and return the
    /// programs whose head is newer than the build `layout` has installed. A program
    /// excluded ([`Self::set_exclude`]), not installed, removed, held by a local pin or
    /// dev-linked is not asked; the last four are read off `layout` now. A 304 stands
    /// for the head last read, compared with the build installed now, so a head whose pass
    /// did not land it is offered again after [`REOFFER_AFTER`]. An unreachable head is
    /// asked again in [`UNREACHABLE_RETRY`], then at [`CADENCE`]; any other answer is
    /// asked again at [`CADENCE`].
    pub fn check(
        &mut self,
        layout: &Layout,
        now: SystemTime,
        get: &VendorGetFn<'_>,
    ) -> Vec<&'static str> {
        if self.shared {
            self.shared_prefix = Some(layout.prefix.clone());
        }
        let Self {
            watched,
            exclude,
            notes,
            shared,
            ..
        } = self;
        let mut moved = Vec::new();
        for (spec, w) in watched.iter_mut() {
            if Self::excludes(exclude, spec.program) {
                continue;
            }
            let (outcome, fresh_notes) = check_one(layout, spec, w, now, get, *shared);
            notes.extend(fresh_notes);
            if outcome.moved {
                moved.push(spec.program);
            }
        }
        moved
    }

    /// Start up to two independent due heads, or harvest those already finished.
    /// A ready vendor is returned immediately while its peer remains in flight, so
    /// the package lane can install it without waiting for the peer's 15 s timeout.
    /// One round finishes before another starts; each store-scoped watch lock
    /// remains owned by the worker doing that vendor's GET. Notes wait for the
    /// round and are then emitted in vendor-table order.
    pub fn check_pending(
        &mut self,
        layout: &Layout,
        now: SystemTime,
        get: &Arc<ConcurrentVendorGetFn<'static>>,
    ) -> Vec<&'static str> {
        self.check_pending_with_index_in_flight(layout, now, get, false)
    }

    /// The GUI may be waiting for an independent index probe. Every due vendor
    /// GET uses an owned worker, even when it is the only one: a slow vendor
    /// must not delay a local bump, full-pass deadline, or switch response.
    /// `index_in_flight` remains in this entry point for its existing callers.
    pub fn check_pending_with_index_in_flight(
        &mut self,
        layout: &Layout,
        now: SystemTime,
        get: &Arc<ConcurrentVendorGetFn<'static>>,
        _index_in_flight: bool,
    ) -> Vec<&'static str> {
        if self.shared {
            self.shared_prefix = Some(layout.prefix.clone());
        }
        let deferred = self.take_deferred_moved(layout);
        if !deferred.is_empty() {
            return deferred;
        }
        let mut notices = std::mem::take(&mut self.ready_notices);
        while let Ok(index) = self.completed_rx.try_recv() {
            notices.push(index);
        }
        let mut moved = Vec::new();
        let mut harvested = false;
        for index in 0..self.pending.len() {
            let ready = self.pending[index]
                .as_ref()
                .is_some_and(|pending| pending.handle.is_finished() || notices.contains(&index));
            if !ready {
                continue;
            }
            harvested = true;
            let pending = self.pending[index].take().expect("ready vendor head");
            match pending.handle.join() {
                Ok(completed) => {
                    debug_assert_eq!(completed.index, index);
                    let mut watched = completed.watched;
                    if let Some(after_wake) = pending.wake_after {
                        watched.next_check_at = Some(after_wake);
                    }
                    self.watched[index].1 = watched;
                    self.round_notes[index] = Some(completed.notes);
                    // A whole-store pass may have landed this build while the
                    // hint was in flight (or the user may have removed/held the
                    // program). Recheck the live store before spawning a second,
                    // now-redundant targeted update.
                    let still_needs_pass =
                        !Self::excludes(&self.exclude, self.watched[index].0.program)
                            && self.watched[index].1.offered.is_some_and(|(head, _)| {
                                installed(layout, self.watched[index].0)
                                    .is_some_and(|active| active.older_than(head))
                            });
                    if completed.outcome.moved && still_needs_pass {
                        moved.push(self.watched[index].0.program);
                    }
                }
                Err(_) => {
                    // A panicked optional hint worker loses no install authority.
                    // The next normal cadence asks again without spawning a new
                    // worker beside a still-running one.
                    self.watched[index].1.next_check_at = Some(now + CADENCE);
                    self.round_notes[index] = Some(vec![format!(
                        "vendor head worker unavailable: {}",
                        self.watched[index].0.program
                    )]);
                }
            }
        }
        self.finish_round_if_done();
        if harvested {
            return moved;
        }
        if self.has_pending() {
            return Vec::new();
        }
        let due: Vec<usize> = self
            .watched
            .iter()
            .enumerate()
            .filter_map(|(index, (spec, watched))| {
                (!Self::excludes(&self.exclude, spec.program)
                    && watched.due(now)
                    && installed(layout, spec).is_some())
                .then_some(index)
            })
            .take(2)
            .collect();
        // The empty case still walks `check_one` so uninstalled vendors get a
        // cadence deadline instead of being rechecked on every park slice.
        if due.is_empty() {
            return self.check(layout, now, get.as_ref());
        }
        self.round_active = true;
        let shared = self.shared;
        for index in due {
            let spec = self.watched[index].0;
            let mut snapshot = self.watched[index].1.clone();
            let layout_copy = layout.clone();
            let get_copy = Arc::clone(get);
            let completed_tx = self.completed_tx.clone();
            let fail_spawn = {
                #[cfg(test)]
                {
                    std::mem::take(&mut self.fail_next_pending_spawn)
                }
                #[cfg(not(test))]
                {
                    false
                }
            };
            let worker = if fail_spawn {
                Err(std::io::Error::other("injected vendor helper failure"))
            } else {
                std::thread::Builder::new()
                    .name(format!("atpkg-head-{}", spec.program))
                    .spawn(move || {
                        let (outcome, notes) = check_one(
                            &layout_copy,
                            spec,
                            &mut snapshot,
                            now,
                            get_copy.as_ref(),
                            shared,
                        );
                        let _ = completed_tx.send(index);
                        CompletedHead {
                            index,
                            watched: snapshot,
                            outcome,
                            notes,
                        }
                    })
            };
            match worker {
                Ok(handle) => {
                    self.pending[index] = Some(PendingHead {
                        handle,
                        wake_after: None,
                    });
                }
                Err(error) => {
                    // Thread creation can fail under host resource pressure.
                    // Keep the package lane available for the index answer or
                    // full-pass deadline; retry the optional vendor hint soon.
                    self.watched[index].1.next_check_at = Some(now + BUSY_RETRY);
                    self.round_notes[index] = Some(vec![format!(
                        "vendor head helper unavailable: {} ({error})",
                        spec.program
                    )]);
                }
            }
        }
        self.finish_round_if_done();
        moved
    }

    fn finish_round_if_done(&mut self) {
        if !self.round_active || self.has_pending() {
            return;
        }
        for notes in &mut self.round_notes {
            if let Some(notes) = notes.take() {
                self.notes.extend(notes);
            }
        }
        self.round_active = false;
    }

    /// A targeted pass which did not complete may retry the same head once after five
    /// minutes. The next failure returns to the hourly offer; a different head resets
    /// that budget. The pass remains the sole authority for installation.
    pub fn note_pass_result(&mut self, program: &str, success: bool, now: SystemTime) {
        let Some((spec, watched)) = self.watched.iter_mut().find(|(s, _)| s.program == program)
        else {
            return;
        };
        let Some(result) = watched.result_for_current_offer(success, now) else {
            return;
        };
        if !self.shared {
            watched.apply_result(result);
            return;
        }
        watched.deferred_result = Some(result);
        let Some(prefix) = &self.shared_prefix else {
            return;
        };
        let layout = Layout {
            prefix: prefix.clone(),
        };
        let mut file = match open_shared(&layout, spec) {
            SharedFile::Held(file) => file,
            SharedFile::Busy => {
                watched.next_check_at = Some(now + BUSY_RETRY);
                return;
            }
            SharedFile::Unavailable => {
                // A read-only or missing store watch file falls back to the
                // process-local watcher. Keep the deferred result as well, so a
                // later recovered file can still reconcile the exact offer.
                watched.apply_result(result);
                return;
            }
        };
        let mut state = read_shared(&mut file, spec);
        let mut current = state.watched();
        if current.matches_result(result) {
            current.apply_result(result);
            state.remember(&current);
            if write_shared(&mut file, &state).is_err() {
                watched.next_check_at = Some(now + BUSY_RETRY);
                return;
            }
        }
        *watched = current;
    }

    /// Whether this store actually reached the version the watch offered. A vendor
    /// pass may exit successfully when the host became unreachable after the hint;
    /// that is eligible for the same one short retry as a failed child.
    #[must_use]
    pub fn head_reached(&self, layout: &Layout, program: &str) -> bool {
        let Some((spec, watched)) = self.watched.iter().find(|(s, _)| s.program == program) else {
            return false;
        };
        let Some((offered, _)) = watched.offered else {
            return false;
        };
        installed(layout, spec).is_some_and(|active| !active.older_than(offered))
    }

    /// One log line each since the last call: a head that moved, a head still ahead of the
    /// build and offered again, a head that became unreachable, a head reachable again.
    /// Never one per check.
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }
}

#[derive(Debug, Default)]
struct CheckOutcome {
    moved: bool,
    reached: bool,
}

fn check_one(
    layout: &Layout,
    spec: &'static VendorSpec,
    watched: &mut Watched,
    now: SystemTime,
    get: &VendorGetFn<'_>,
    shared: bool,
) -> (CheckOutcome, Vec<String>) {
    let mut notes = Vec::new();
    if !watched.due(now) {
        return (CheckOutcome::default(), notes);
    }
    let Some(installed) = installed(layout, spec) else {
        watched.next_check_at = Some(now + CADENCE);
        return (CheckOutcome::default(), notes);
    };
    let outcome = if shared {
        check_shared(layout, spec, watched, installed, now, get, &mut notes)
    } else {
        check_watched(spec, watched, installed, now, get, &mut notes)
    };
    (outcome, notes)
}

/// The original per-vendor decision, now driven either by a process-local state (test or
/// read-only-store fallback) or by the state read under the store-scoped watch lock.
fn check_watched(
    spec: &VendorSpec,
    watched: &mut Watched,
    installed: Installed,
    now: SystemTime,
    get: &VendorGetFn<'_>,
    notes: &mut Vec<String>,
) -> CheckOutcome {
    // A short retry deadline can fall just after a normal minute tick. Reoffer
    // the recently checked head without launching another curl subprocess.
    if let Some(last) = watched.last_fetch_at
        && now.duration_since(last).is_ok_and(|age| age < CADENCE)
    {
        watched.next_check_at = Some(last + CADENCE);
        return offer_cached(spec, watched, installed, now, notes, false);
    }
    watched.next_check_at = Some(now + CADENCE);
    watched.last_fetch_at = Some(now);
    let program = spec.program;
    let etag = watched.head.and(watched.etag.clone());
    let answer = resolve::read_head_via(spec, get, etag.as_deref());
    if let Head::Unreachable(why) = &answer {
        watched.unreachable = watched.unreachable.saturating_add(1);
        if watched.unreachable == 1 {
            watched.next_check_at = Some(now + UNREACHABLE_RETRY);
            notes.push(format!("vendor head unreachable: {program} ({why})"));
        }
        return CheckOutcome::default();
    }
    if watched.unreachable > 0 {
        watched.unreachable = 0;
        notes.push(format!("vendor head reachable again: {program}"));
    }
    let (head, reached) = match answer {
        Head::Unchanged { etag } => {
            watched.etag = Some(etag);
            (watched.head, true)
        }
        Head::Named { version, etag, .. } => {
            watched.head = Some(version);
            watched.etag = etag;
            (Some(version), true)
        }
        Head::Unreachable(_) | Head::Unserved(_) | Head::Refused(_) => (None, false),
    };
    if head.is_none() {
        return CheckOutcome {
            moved: false,
            reached,
        };
    }
    offer_cached(spec, watched, installed, now, notes, reached)
}

fn offer_cached(
    spec: &VendorSpec,
    watched: &mut Watched,
    installed: Installed,
    now: SystemTime,
    notes: &mut Vec<String>,
    reached: bool,
) -> CheckOutcome {
    let Some(head) = watched.head.filter(|&h| installed.older_than(h)) else {
        return CheckOutcome {
            moved: false,
            reached,
        };
    };
    let Some(again) = watched.offer(head, now) else {
        return CheckOutcome {
            moved: false,
            reached,
        };
    };
    let verb = if again { "still ahead" } else { "moved" };
    notes.push(format!(
        "vendor head {verb}: {} {head} ({})",
        spec.program,
        installed.words()
    ));
    CheckOutcome {
        moved: true,
        reached,
    }
}

enum SharedFile {
    /// Locked; the drop releases it at once ([`crate::lock::Flock`]).
    Held(crate::lock::Flock),
    Busy,
    Unavailable,
}

fn open_shared(layout: &Layout, spec: &VendorSpec) -> SharedFile {
    let path = layout
        .prefix
        .join(format!("vendor-head-{}.watch", spec.program));
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let Ok(file) = options.open(path) else {
        return SharedFile::Unavailable;
    };
    let Ok(metadata) = file.metadata() else {
        return SharedFile::Unavailable;
    };
    if !metadata.file_type().is_file() || crate::platform::is_reparse(&metadata) {
        return SharedFile::Unavailable;
    }
    match crate::lock::Flock::try_lock(file) {
        Ok(file) => SharedFile::Held(file),
        Err(std::fs::TryLockError::WouldBlock) => SharedFile::Busy,
        Err(std::fs::TryLockError::Error(_)) => SharedFile::Unavailable,
    }
}

fn read_shared(file: &mut File, spec: &VendorSpec) -> SharedState {
    let empty = || SharedState::empty(spec);
    if file.metadata().map_or(true, |m| m.len() > MAX_SHARED_BYTES) {
        return empty();
    }
    let mut text = String::new();
    if file.seek(std::io::SeekFrom::Start(0)).is_err()
        || file
            .take(MAX_SHARED_BYTES)
            .read_to_string(&mut text)
            .is_err()
    {
        return empty();
    }
    let Ok(state): Result<SharedState, _> = aterm_toml::from_str(&text) else {
        return empty();
    };
    if state.schema != SHARED_SCHEMA
        || state.source != spec.head_url
        || state.etag.as_ref().is_some_and(|e| {
            e.is_empty() || e.len() > 256 || !e.bytes().all(|b| (0x20..0x7f).contains(&b))
        })
    {
        return empty();
    }
    state
}

fn write_shared(file: &mut File, state: &SharedState) -> std::io::Result<()> {
    let text = aterm_toml::to_string(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    if text.len() as u64 > MAX_SHARED_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "vendor head watch state exceeds its size limit",
        ));
    }
    file.set_len(0)?;
    file.seek(std::io::SeekFrom::Start(0))?;
    file.write_all(text.as_bytes())?;
    file.flush()
}

fn check_shared(
    layout: &Layout,
    spec: &VendorSpec,
    watched: &mut Watched,
    installed: Installed,
    now: SystemTime,
    get: &VendorGetFn<'_>,
    notes: &mut Vec<String>,
) -> CheckOutcome {
    let mut file = match open_shared(layout, spec) {
        SharedFile::Held(file) => file,
        SharedFile::Busy => {
            watched.next_check_at = Some(now + BUSY_RETRY);
            return CheckOutcome::default();
        }
        SharedFile::Unavailable => {
            if let Some(result) = watched.deferred_result
                && watched.matches_result(result)
            {
                watched.apply_result(result);
            }
            return check_watched(spec, watched, installed, now, get, notes);
        }
    };
    let mut state = read_shared(&mut file, spec);
    let mut shared_watched = state.watched();
    // The child may have finished while another window held this lock. Reconcile
    // the exact offer's result durably BEFORE the no-GET early return; a newer
    // offer of even the same version must not inherit the old child's result.
    if let Some(result) = watched.deferred_result {
        if shared_watched.matches_result(result) {
            shared_watched.apply_result(result);
            state.remember(&shared_watched);
            if write_shared(&mut file, &state).is_err() {
                watched.next_check_at = Some(now + BUSY_RETRY);
                return CheckOutcome::default();
            }
        }
        watched.deferred_result = None;
    }
    if !shared_watched.due(now) {
        *watched = shared_watched;
        return CheckOutcome::default();
    }
    let outcome = check_watched(spec, &mut shared_watched, installed, now, get, notes);
    if outcome.reached
        && let (Some(checked_at), Some(head)) = (unix_seconds(now), shared_watched.head)
        && (state.head != Some(head)
            || state.last_record_at.is_none_or(|last| {
                checked_at < last || checked_at - last >= DURABLE_CHECK_INTERVAL.as_secs()
            }))
        && super::ProgramStamp::record_check(
            layout,
            spec.program,
            i64::try_from(checked_at).unwrap_or(i64::MAX),
            Some(head),
            shared_watched.etag.as_deref(),
        )
        .is_ok()
    {
        // A 304 each minute does not need a durable temp+fsync+rename each minute.
        // The latest-known label lasts 24 h; an hourly refresh is ample.
        state.last_record_at = Some(checked_at);
    }
    state.remember(&shared_watched);
    let _ = write_shared(&mut file, &state);
    *watched = shared_watched;
    outcome
}

/// The watch this process may run, with the GET it reads heads through — `None` when the
/// lane it hints for would reach no vendor ([`admitted`]). `exclude` is `[packages].exclude`
/// at the start; a long-lived caller keeps it current ([`HeadWatch::set_exclude`]).
#[must_use]
pub fn for_this_process(
    exclude: &[String],
) -> Option<(HeadWatch, Arc<ConcurrentVendorGetFn<'static>>)> {
    admitted(
        crate::manager_enabled(),
        crate::cli::registry_seam().as_deref(),
    )
    .then(|| {
        (
            HeadWatch::shared(exclude),
            Arc::new(network_get) as Arc<ConcurrentVendorGetFn<'static>>,
        )
    })
}

/// Whether the heads may be watched: the manager is on (not an unpinned root), and the
/// pass's fetcher is the network's — a development build's `dir:` registry never reaches a
/// vendor ([`crate::cli::dir_registry`]). `[packages] enabled` is the caller's gate: the
/// window starts no watch while automatic updates are off.
fn admitted(manager_enabled: bool, registry: Option<&OsStr>) -> bool {
    manager_enabled && crate::cli::dir_registry(registry).is_none()
}

/// The production GET: the lane's own pins, caps, anonymity and CA-trust scrub, in one
/// short attempt ([`crate::net::vendor_hint_get`]) — the next check is the retry.
///
/// # Errors
/// As [`crate::flow::Fetcher::vendor_get`].
pub fn network_get(
    program: &str,
    url: &str,
    cap: u64,
    if_none_match: Option<&str>,
) -> Result<VendorGet, VendorFetchError> {
    crate::net::vendor_hint_get(program, url, cap, if_none_match)
}

/// Whether `layout`'s copy of `spec`'s program is one the watch keeps at its vendor's head:
/// installed, and not removed, held or dev-linked ([`installed`]). `aterm pkg doctor`'s
/// never-checked line names only these as updating without an index pass.
pub(crate) fn follows_vendor(layout: &Layout, spec: &VendorSpec) -> bool {
    installed(layout, spec).is_some()
}

/// What `layout` has installed of `spec`'s program through its `store/<program>/current`
/// link, or `None` when the watch leaves it alone: not installed, removed, held by a local
/// pin, or dev-linked.
fn installed(layout: &Layout, spec: &VendorSpec) -> Option<Installed> {
    let program = spec.program;
    if layout.removed_programs().contains(program)
        || crate::pin::is_pinned(layout, program)
        || crate::linkmode::is_linked(layout, program)
    {
        return None;
    }
    let target = std::fs::read_link(layout.program_current(program)).ok()?;
    let (owner, build) = crate::ops::store_build_of(&layout.prefix, &target)?;
    let dir = layout.build_dir(program, build);
    if owner != program || !dir.is_dir() {
        return None;
    }
    Some(if is_vendor_build(build) {
        super::complete_record(&dir).map_or(Installed::Unknown, |r| Installed::Version(r.version))
    } else {
        // A legacy build's version the lane read and kept: one at the head is no trigger.
        super::ProgramStamp::read(layout, program)
            .and_then(|s| s.legacy_version_of(build))
            .map_or(Installed::Unknown, Installed::Version)
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};

    use super::*;
    use crate::vendor_direct::{Anchor, RECORD_SCHEMA, VendorRecord, record_path, spec};

    const CLAUDE_HEAD: &str = "https://downloads.claude.ai/claude-code-releases/latest";
    const CODEX_HEAD: &str = "https://releases.openai.com/codex/channels/latest";

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    /// The wall clock, `secs` after a fixed origin.
    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000 + secs)
    }

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-watch-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Layout { prefix: p }
    }

    /// Point `store/<program>/current` at `build`'s directory, creating it.
    fn activate(layout: &Layout, program: &str, build: u64) -> PathBuf {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        crate::activate::atomic_symlink(&dir, &layout.program_current(program)).unwrap();
        dir
    }

    /// A complete, recorded vendor build of `program` at `version`, active.
    fn install(layout: &Layout, program: &str, version: &str) {
        let spec = spec(program).unwrap();
        let version = v(version);
        let dir = activate(layout, program, version.build_id());
        let source_url = match spec.anchor {
            Anchor::AnthropicOpenPgp => {
                format!(
                    "https://downloads.claude.ai/claude-code-releases/{version}/darwin-arm64/claude"
                )
            }
            Anchor::OpenAiTwoHost => format!(
                "https://github.com/openai/codex/releases/download/rust-v{version}/codex-package-aarch64-apple-darwin.tar.gz"
            ),
        };
        let record = VendorRecord {
            schema: RECORD_SCHEMA,
            program: program.to_string(),
            version,
            vendor: spec.vendor.to_string(),
            source_url,
            sha256: "a".repeat(64),
            size: 1,
            tree_root: "b".repeat(64),
            apple_team: cfg!(target_os = "macos").then(|| spec.apple_team.to_string()),
            anchor: spec.anchor,
            build_date: None,
            verified_at: 1_790_000_000,
        };
        let bytes = record.record_bytes().unwrap();
        super::super::durable::write_durable(&record_path(&dir).unwrap(), &bytes).unwrap();
        crate::store::mark_build_ready(&dir).unwrap();
        assert_eq!(
            super::super::complete_record(&dir).map(|r| r.version),
            Some(version)
        );
    }

    /// What one GET answered.
    #[derive(Clone)]
    enum Answer {
        Body(Vec<u8>, Option<String>),
        Unreachable,
        Unserved,
    }

    /// The vendors, as the watch's GET sees them. A body served with an ETag answers a
    /// matching `If-None-Match` with a 304.
    #[derive(Default)]
    struct Vendors {
        heads: RefCell<BTreeMap<&'static str, Answer>>,
        /// Every GET: `(url, if_none_match)`.
        gets: RefCell<Vec<(String, Option<String>)>>,
    }

    impl Vendors {
        fn head(&self, url: &'static str, body: &str, etag: Option<&str>) {
            self.heads.borrow_mut().insert(
                url,
                Answer::Body(body.as_bytes().to_vec(), etag.map(str::to_string)),
            );
        }

        fn answer(&self, url: &'static str, answer: Answer) {
            self.heads.borrow_mut().insert(url, answer);
        }

        fn codex(&self, version: &str, etag: Option<&str>) {
            self.head(
                CODEX_HEAD,
                &format!("{{\"tag_name\":\"rust-v{version}\",\"assets\":[]}}"),
                etag,
            );
        }

        fn get(
            &self,
            program: &str,
            url: &str,
            cap: u64,
            if_none_match: Option<&str>,
        ) -> Result<VendorGet, VendorFetchError> {
            // The lane's pins and the resolver's head caps, exactly.
            assert!(
                crate::vendor::vendor_direct_url_allowed(program, url),
                "{url}"
            );
            let want = if program == "claude" { 64 } else { 1024 * 1024 };
            assert_eq!(cap, want, "{program}'s head cap");
            self.gets
                .borrow_mut()
                .push((url.to_string(), if_none_match.map(str::to_string)));
            match self.heads.borrow().get(url).cloned() {
                Some(Answer::Body(bytes, etag)) => {
                    if etag.is_some() && etag.as_deref() == if_none_match {
                        return Ok(VendorGet::NotModified {
                            etag: etag.unwrap_or_default(),
                        });
                    }
                    Ok(VendorGet::Body {
                        bytes,
                        etag,
                        effective_url: url.to_string(),
                    })
                }
                Some(Answer::Unreachable) => {
                    Err(VendorFetchError::Unreachable(String::from("timed out")))
                }
                Some(Answer::Unserved) | None => {
                    Err(VendorFetchError::Refused(String::from("HTTP 404")))
                }
            }
        }

        fn take_gets(&self) -> Vec<(String, Option<String>)> {
            std::mem::take(&mut self.gets.borrow_mut())
        }
    }

    fn check(watch: &mut HeadWatch, l: &Layout, f: &Vendors, now: SystemTime) -> Vec<&'static str> {
        watch.check(l, now, &|p, u, c, e| f.get(p, u, c, e))
    }

    #[test]
    fn a_newer_head_triggers_and_an_equal_or_older_one_does_not() {
        let l = layout("newer");
        install(&l, "claude", "2.1.280");
        install(&l, "codex", "0.156.0");
        let f = Vendors::default();
        f.head(CLAUDE_HEAD, "2.1.281\n", Some("\"c1\""));
        f.codex("0.156.0", Some("\"x1\""));
        let mut w = HeadWatch::new(&[]);
        assert!(w.due(at(0)), "every head is due at once");
        assert_eq!(check(&mut w, &l, &f, at(0)), ["claude"]);
        assert_eq!(
            w.take_notes(),
            ["vendor head moved: claude 2.1.281 (installed 2.1.280)"]
        );
        assert_eq!(
            f.take_gets(),
            [
                (CLAUDE_HEAD.to_string(), None),
                (CODEX_HEAD.to_string(), None)
            ]
        );
        // An older head is never a trigger (the lane never downgrades on a head).
        let mut w = HeadWatch::new(&[]);
        f.head(CLAUDE_HEAD, "2.1.279", None);
        f.codex("0.155.0", None);
        assert!(check(&mut w, &l, &f, at(0)).is_empty());
        assert!(w.take_notes().is_empty());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn concurrent_watch_reaches_codex_while_claude_head_is_waiting() {
        let l = layout("concurrent-heads");
        install(&l, "claude", "2.1.280");
        install(&l, "codex", "0.156.0");
        let release_claude = Arc::new((Mutex::new(false), Condvar::new()));
        let claude_finished = Arc::new(AtomicBool::new(false));
        let gate = Arc::clone(&release_claude);
        let finished = Arc::clone(&claude_finished);
        let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(
            move |program: &str, url: &str, cap: u64, etag: Option<&str>| {
                assert!(etag.is_none());
                let bytes = if program == "claude" {
                    assert_eq!((url, cap), (CLAUDE_HEAD, 64));
                    let (lock, ready) = &*gate;
                    let released = lock.lock().unwrap();
                    let (released, _) = ready
                        .wait_timeout_while(released, Duration::from_secs(30), |released| {
                            !*released
                        })
                        .unwrap();
                    assert!(*released, "test release arrived before the network timeout");
                    finished.store(true, Ordering::Release);
                    b"2.1.281".to_vec()
                } else {
                    assert_eq!((program, url, cap), ("codex", CODEX_HEAD, 1024 * 1024));
                    br#"{"tag_name":"rust-v0.157.0","assets":[]}"#.to_vec()
                };
                Ok(VendorGet::Body {
                    bytes,
                    etag: None,
                    effective_url: url.to_string(),
                })
            },
        );
        let mut watch = HeadWatch::shared(&[]);
        assert!(watch.check_pending(&l, at(0), &get).is_empty());
        assert_eq!(
            watch.pending_count(),
            2,
            "both independent hints are in flight"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let moved = loop {
            watch.park_for_hint(Duration::from_millis(100));
            let moved = watch.check_pending(&l, at(0), &get);
            if !moved.is_empty() || std::time::Instant::now() >= deadline {
                break moved;
            }
        };
        assert_eq!(moved, ["codex"], "Codex is offered before Claude finishes");
        assert!(
            !claude_finished.load(Ordering::Acquire),
            "the slow Claude head is still waiting"
        );
        assert_eq!(watch.pending_count(), 1);
        assert!(
            watch.take_notes().is_empty(),
            "notes await table-order flush"
        );
        assert!(
            watch.check_pending(&l, at(0), &get).is_empty(),
            "no duplicate offer"
        );
        let (lock, ready) = &*release_claude;
        *lock.lock().unwrap() = true;
        ready.notify_all();
        watch.park_for_hint(Duration::from_secs(5));
        assert_eq!(watch.check_pending(&l, at(0), &get), ["claude"]);
        assert_eq!(watch.pending_count(), 0);
        let notes = watch.take_notes();
        assert!(
            notes[0].starts_with("vendor head moved: claude"),
            "{notes:?}"
        );
        assert!(
            notes[1].starts_with("vendor head moved: codex"),
            "{notes:?}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A single installed vendor used to run its GET inline. A blocked host
    /// then held the package lane for the hint's whole 15-second network bound,
    /// so a bump or full-pass deadline arriving meanwhile could not be read.
    #[test]
    fn one_blocked_vendor_is_owned_without_blocking_the_package_lane() {
        let l = layout("one-blocked-head");
        install(&l, "claude", "2.1.280");
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let wait_gate = Arc::clone(&gate);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(move |program, url, cap, etag| {
            assert_eq!((program, url, cap, etag), ("claude", CLAUDE_HEAD, 64, None));
            started_tx.send(()).unwrap();
            let (lock, ready) = &*wait_gate;
            let released = lock.lock().unwrap();
            let (released, _) = ready
                .wait_timeout_while(released, Duration::from_secs(30), |released| !*released)
                .unwrap();
            assert!(*released, "the test released the simulated vendor GET");
            Ok(VendorGet::Body {
                bytes: b"2.1.281".to_vec(),
                etag: None,
                effective_url: url.to_string(),
            })
        });
        let caller_layout = l.clone();
        let caller_get = Arc::clone(&get);
        let (returned_tx, returned_rx) = std::sync::mpsc::channel();
        let caller = std::thread::spawn(move || {
            let mut watch = HeadWatch::shared(&[String::from("codex")]);
            let moved = watch.check_pending(&caller_layout, at(0), &caller_get);
            returned_tx.send((watch, moved)).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let early = returned_rx.recv_timeout(Duration::from_secs(2));
        let returned_while_blocked = early.is_ok();
        let owned_while_blocked = early
            .as_ref()
            .is_ok_and(|(watch, moved)| moved.is_empty() && watch.pending_count() == 1);
        // Release on both sides of the regression so the old inline behavior
        // fails promptly instead of leaving its caller blocked for 30 seconds.
        let (lock, ready) = &*gate;
        *lock.lock().unwrap() = true;
        ready.notify_all();
        let (mut watch, moved) =
            early.unwrap_or_else(|_| returned_rx.recv_timeout(Duration::from_secs(5)).unwrap());
        caller.join().unwrap();
        assert!(
            returned_while_blocked,
            "one slow vendor GET blocked the package lane"
        );
        assert!(owned_while_blocked, "the blocked GET must remain owned");
        assert!(moved.is_empty());
        watch.park_for_hint(Duration::from_secs(5));
        assert_eq!(watch.check_pending(&l, at(0), &get), ["claude"]);
        assert_eq!(watch.pending_count(), 0);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn one_vendor_spawn_failure_retries_without_an_inline_get() {
        let l = layout("one-spawn-failed-head");
        install(&l, "claude", "2.1.280");
        let calls = Arc::new(AtomicUsize::new(0));
        let get_calls = Arc::clone(&calls);
        let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(move |program, url, _, _| {
            assert_eq!((program, url), ("claude", CLAUDE_HEAD));
            get_calls.fetch_add(1, Ordering::SeqCst);
            Ok(VendorGet::Body {
                bytes: b"2.1.281".to_vec(),
                etag: None,
                effective_url: url.to_string(),
            })
        });
        let mut watch = HeadWatch::new(&[String::from("codex")]);
        watch.fail_next_pending_spawn = true;
        assert!(watch.check_pending(&l, at(0), &get).is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(watch.pending_count(), 0);
        assert!(!watch.round_active);
        assert_eq!(watch.watched[0].1.next_check_at, Some(at(0) + BUSY_RETRY));
        assert!(
            watch
                .take_notes()
                .iter()
                .any(|note| note.starts_with("vendor head helper unavailable: claude"))
        );
        assert!(watch.check_pending(&l, at(0) + BUSY_RETRY, &get).is_empty());
        assert_eq!(watch.pending_count(), 1);
        watch.park_for_hint(Duration::from_secs(2));
        assert_eq!(
            watch.check_pending(&l, at(0) + BUSY_RETRY, &get),
            ["claude"]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn a_deferred_hint_is_skipped_if_the_full_pass_installed_it() {
        let l = layout("deferred-head-landed");
        install(&l, "claude", "2.1.280");
        let calls = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&calls);
        let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(move |program, url, _, _| {
            assert_eq!((program, url), ("claude", CLAUDE_HEAD));
            count.fetch_add(1, Ordering::SeqCst);
            Ok(VendorGet::Body {
                bytes: b"2.1.281".to_vec(),
                etag: None,
                effective_url: url.to_string(),
            })
        });
        let mut watch = HeadWatch::new(&[String::from("codex")]);
        let moved = watch.check(&l, at(0), get.as_ref());
        assert_eq!(moved, ["claude"]);
        watch.defer_moved(&moved);
        assert!(watch.due(at(0)), "the carried hint wakes the next park");
        install(&l, "claude", "2.1.281");
        assert!(
            watch.check_pending(&l, at(0), &get).is_empty(),
            "the signed pass already landed the offered version"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no duplicate GET");
        assert!(!watch.due(at(0)), "the carried hint is consumed once");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn vendor_spawn_failure_cannot_block_an_in_flight_index_answer() {
        let model = aterm_spec::derive::atpkg_index_pending_park_model();
        let mut state = model.init_state();
        assert!(model.fire("Start", &mut state));
        let l = layout("spawn-failed-beside-index");
        install(&l, "claude", "2.1.280");
        let calls = Arc::new(AtomicUsize::new(0));
        let get_calls = Arc::clone(&calls);
        let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(move |program, url, _, _| {
            assert_eq!((program, url), ("claude", CLAUDE_HEAD));
            get_calls.fetch_add(1, Ordering::SeqCst);
            Ok(VendorGet::Body {
                bytes: b"2.1.281".to_vec(),
                etag: None,
                effective_url: url.to_string(),
            })
        });
        let mut watch = HeadWatch::new(&[String::from("codex")]);
        watch.fail_next_pending_spawn = true;
        assert!(
            watch
                .check_pending_with_index_in_flight(&l, at(0), &get, true)
                .is_empty()
        );
        assert!(model.fire("VendorSpawnFailed", &mut state));
        assert_eq!(
            i64::try_from(calls.load(Ordering::SeqCst)).unwrap(),
            state["inline_vendor_get"]
        );
        assert_eq!(watch.pending_count(), 0);
        assert!(!watch.round_active, "a failed spawn leaves no active round");
        assert_eq!(watch.watched[0].1.next_check_at, Some(at(0) + BUSY_RETRY));
        assert!(!watch.due(at(0) + BUSY_RETRY - Duration::from_millis(1)));
        assert!(watch.due(at(0) + BUSY_RETRY));
        assert_eq!(state["vendor_retry"], 1);
        assert!(
            watch
                .take_notes()
                .iter()
                .any(|note| note.starts_with("vendor head helper unavailable: claude"))
        );

        // The short retry can still deliver the vendor hint through its owned
        // worker slot; the index probe need not finish first.
        assert!(
            watch
                .check_pending_with_index_in_flight(&l, at(0) + BUSY_RETRY, &get, true)
                .is_empty()
        );
        assert_eq!(watch.pending_count(), 1);
        watch.park_for_hint(Duration::from_secs(2));
        assert_eq!(
            watch.check_pending_with_index_in_flight(&l, at(0) + BUSY_RETRY, &get, true),
            ["claude"]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(model.fire("VendorReady", &mut state));
        assert_eq!(watch.pending_count(), 0);

        // Negative control: the model of the old inline fallback has one GET
        // during the in-flight index probe and violates this same projection.
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut old = buggy.init_state();
        assert!(buggy.fire("Start", &mut old));
        assert!(buggy.fire("VendorSpawnFailed", &mut old));
        assert_eq!(old["inline_vendor_get"], 1);
        assert!(!buggy.check_invariant("NoInlineVendorGetOnHelperFailure", &old));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn vendor_spawn_failure_defers_even_without_an_index_probe() {
        let model = aterm_spec::derive::atpkg_index_pending_park_model();
        let mut state = model.init_state();
        let l = layout("spawn-failed-without-index");
        install(&l, "claude", "2.1.280");
        install(&l, "codex", "0.156.0");
        let claude_calls = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&claude_calls);
        let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(move |program, url, _, _| {
            let bytes = if program == "claude" {
                assert_eq!(url, CLAUDE_HEAD);
                count.fetch_add(1, Ordering::SeqCst);
                b"2.1.281".to_vec()
            } else {
                assert_eq!((program, url), ("codex", CODEX_HEAD));
                br#"{"tag_name":"rust-v0.156.0","assets":[]}"#.to_vec()
            };
            Ok(VendorGet::Body {
                bytes,
                etag: None,
                effective_url: url.to_string(),
            })
        });
        let mut watch = HeadWatch::new(&[]);
        watch.fail_next_pending_spawn = true;
        assert!(watch.check_pending(&l, at(0), &get).is_empty());
        assert!(model.fire("VendorSpawnFailed", &mut state));
        assert_eq!(claude_calls.load(Ordering::SeqCst), 0);
        assert_eq!(state["inline_vendor_get"], 0);
        assert_eq!(watch.pending_count(), 1, "the other vendor keeps its slot");
        watch.park_for_hint(Duration::from_secs(2));
        assert!(watch.check_pending(&l, at(0), &get).is_empty());
        assert_eq!(watch.pending_count(), 0);
        assert!(!watch.round_active);
        assert_eq!(watch.watched[0].1.next_check_at, Some(at(0) + BUSY_RETRY));
        assert!(!watch.due(at(0) + BUSY_RETRY - Duration::from_millis(1)));
        assert!(watch.due(at(0) + BUSY_RETRY));

        // The failed hint remains available at its short retry, still on an
        // owned worker now that it is the only due vendor.
        assert!(watch.check_pending(&l, at(0) + BUSY_RETRY, &get).is_empty());
        assert_eq!(watch.pending_count(), 1);
        watch.park_for_hint(Duration::from_secs(2));
        assert_eq!(
            watch.check_pending(&l, at(0) + BUSY_RETRY, &get),
            ["claude"]
        );
        assert_eq!(claude_calls.load(Ordering::SeqCst), 1);
        assert!(model.fire("VendorReady", &mut state));
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut old = buggy.init_state();
        assert!(buggy.fire("VendorSpawnFailed", &mut old));
        assert_eq!(old["inline_vendor_get"], 1);
        assert!(!buggy.check_invariant("NoInlineVendorGetOnHelperFailure", &old));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn completed_hint_skips_a_pass_already_landed_by_the_whole_store_update() {
        let l = layout("head-landed-before-harvest");
        install(&l, "claude", "2.1.280");
        install(&l, "codex", "0.156.0");
        let release_claude = Arc::new((Mutex::new(false), Condvar::new()));
        let (codex_started_tx, codex_started_rx) = std::sync::mpsc::channel();
        let gate = Arc::clone(&release_claude);
        let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(move |program, url, _, _| {
            let bytes = if program == "claude" {
                let (lock, ready) = &*gate;
                let released = lock.lock().unwrap();
                let (released, _) = ready
                    .wait_timeout_while(released, Duration::from_secs(30), |released| !*released)
                    .unwrap();
                assert!(*released);
                b"2.1.281".to_vec()
            } else {
                let _ = codex_started_tx.send(());
                br#"{"tag_name":"rust-v0.157.0","assets":[]}"#.to_vec()
            };
            Ok(VendorGet::Body {
                bytes,
                etag: None,
                effective_url: url.to_string(),
            })
        });
        let mut watch = HeadWatch::new(&[]);
        assert!(watch.check_pending(&l, at(0), &get).is_empty());
        codex_started_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        watch.park_for_hint(Duration::from_secs(5));
        install(&l, "codex", "0.157.0");
        assert!(
            watch.check_pending(&l, at(0), &get).is_empty(),
            "the whole pass reached Codex before the hint was harvested"
        );
        assert_eq!(watch.pending_count(), 1);
        let (lock, ready) = &*release_claude;
        *lock.lock().unwrap() = true;
        ready.notify_all();
        watch.park_for_hint(Duration::from_secs(5));
        assert_eq!(watch.check_pending(&l, at(0), &get), ["claude"]);
        let notes = watch.take_notes();
        assert!(
            notes
                .iter()
                .any(|note| note.starts_with("vendor head moved: codex")),
            "Codex did move before the full pass landed it: {notes:?}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn a_304_is_nothing_and_costs_one_conditional_get() {
        let l = layout("304");
        install(&l, "claude", "2.1.280");
        let f = Vendors::default();
        f.head(CLAUDE_HEAD, "2.1.281", Some("\"c1\""));
        let mut w = HeadWatch::new(&[]);
        assert_eq!(check(&mut w, &l, &f, at(0)), ["claude"]);
        f.take_gets();
        // The pass landed the head: every later check is one conditional GET answered
        // 304, and a 304 over a head the build has reached is nothing — hours on.
        install(&l, "claude", "2.1.281");
        for secs in [300, 600, 3 * 3600] {
            assert!(w.due(at(secs)));
            assert!(check(&mut w, &l, &f, at(secs)).is_empty(), "{secs}");
            assert_eq!(
                f.take_gets(),
                [(CLAUDE_HEAD.to_string(), Some(String::from("\"c1\"")))],
                "{secs}"
            );
        }
        assert_eq!(
            w.take_notes().len(),
            1,
            "the one trigger note, nothing per check"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Separate window processes get distinct HeadWatch values but the same store. Only
    /// one makes each minute's request, and a newly launched one inherits its validator.
    #[test]
    fn independent_watches_share_one_request_and_validator_per_store() {
        let l = layout("shared");
        install(&l, "claude", "2.1.280");
        let f = Vendors::default();
        f.head(CLAUDE_HEAD, "2.1.280", Some("\"c1\""));
        let excluded = [String::from("codex")];
        let mut first = HeadWatch::shared(&excluded);
        let mut second = HeadWatch::shared(&excluded);
        assert!(check(&mut first, &l, &f, at(0)).is_empty());
        let first_record = crate::vendor_direct::ProgramStamp::read(&l, "claude")
            .unwrap()
            .last_checked_at;
        assert_eq!(f.take_gets(), [(CLAUDE_HEAD.to_string(), None)]);
        assert!(
            check(&mut second, &l, &f, at(0) - Duration::from_millis(1)).is_empty(),
            "a slightly earlier sampled clock is not a rollback"
        );
        assert!(check(&mut second, &l, &f, at(5)).is_empty());
        assert!(f.take_gets().is_empty(), "another window reuses the minute");
        assert!(check(&mut second, &l, &f, at(60)).is_empty());
        assert_eq!(
            crate::vendor_direct::ProgramStamp::read(&l, "claude")
                .unwrap()
                .last_checked_at,
            first_record,
            "an unchanged 304 does not fsync the durable vendor stamp every minute"
        );
        assert_eq!(
            f.take_gets(),
            [(CLAUDE_HEAD.to_string(), Some(String::from("\"c1\"")))],
            "the other window inherits the ETag"
        );
        assert!(check(&mut first, &l, &f, at(65)).is_empty());
        assert!(f.take_gets().is_empty());
        f.head(CLAUDE_HEAD, "2.1.281", Some("\"c2\""));
        assert_eq!(check(&mut first, &l, &f, at(120)), ["claude"]);
        assert_eq!(
            f.take_gets(),
            [(CLAUDE_HEAD.to_string(), Some(String::from("\"c1\"")))],
        );
        assert!(check(&mut second, &l, &f, at(125)).is_empty());
        assert!(f.take_gets().is_empty(), "one offer across both windows");
        let mut restarted = HeadWatch::shared(&excluded);
        assert!(check(&mut restarted, &l, &f, at(180)).is_empty());
        assert_eq!(
            f.take_gets(),
            [(CLAUDE_HEAD.to_string(), Some(String::from("\"c2\"")))],
            "a new process inherits the last ETag"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A child can finish while another window holds the short watch lock. Its
    /// result must be written before the next check's not-due early return.
    #[test]
    fn a_result_deferred_by_the_shared_lock_is_reconciled_before_early_return() {
        let l = layout("busy-result");
        install(&l, "claude", "2.1.280");
        let f = Vendors::default();
        f.head(CLAUDE_HEAD, "2.1.281", Some("\"c1\""));
        let spec = spec("claude").unwrap();
        let mut watch = HeadWatch::shared(&[String::from("codex")]);
        assert_eq!(check(&mut watch, &l, &f, at(0)), ["claude"]);
        f.take_gets();
        let SharedFile::Held(mut held) = open_shared(&l, spec) else {
            panic!("watch lock");
        };
        let mut state = read_shared(&mut held, spec);
        state.next_check_at = unix_seconds(at(355));
        write_shared(&mut held, &state).unwrap();
        watch.note_pass_result("claude", false, at(295));
        assert!(watch.watched[0].1.deferred_result.is_some());
        drop(held);
        assert!(check(&mut watch, &l, &f, at(300)).is_empty());
        assert!(f.take_gets().is_empty(), "the shared GET was not due");
        let SharedFile::Held(mut file) = open_shared(&l, spec) else {
            panic!("watch lock");
        };
        let persisted = read_shared(&mut file, spec);
        assert_eq!(persisted.short_retries, 1);
        assert!(!persisted.pending);
        assert_eq!(persisted.offered_at, unix_seconds(at(295)));
        drop(file);
        assert_eq!(check(&mut watch, &l, &f, at(595)), ["claude"]);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A result from an older offer of the *same version* must not clear a newer
    /// offer another process made while the lock was busy.
    #[test]
    fn a_deferred_result_does_not_clear_a_newer_offer_of_the_same_version() {
        let l = layout("stale-result");
        install(&l, "claude", "2.1.280");
        let f = Vendors::default();
        f.head(CLAUDE_HEAD, "2.1.281", Some("\"c1\""));
        let spec = spec("claude").unwrap();
        let mut watch = HeadWatch::shared(&[String::from("codex")]);
        assert_eq!(check(&mut watch, &l, &f, at(0)), ["claude"]);
        f.take_gets();
        let SharedFile::Held(mut held) = open_shared(&l, spec) else {
            panic!("watch lock");
        };
        watch.note_pass_result("claude", false, at(295));
        let mut state = read_shared(&mut held, spec);
        state.offer_generation += 1;
        state.offered_at = unix_seconds(at(295));
        state.next_check_at = unix_seconds(at(355));
        state.short_retries = 1;
        write_shared(&mut held, &state).unwrap();
        drop(held);
        assert!(check(&mut watch, &l, &f, at(300)).is_empty());
        assert!(watch.watched[0].1.deferred_result.is_none());
        let SharedFile::Held(mut file) = open_shared(&l, spec) else {
            panic!("watch lock");
        };
        let persisted = read_shared(&mut file, spec);
        assert!(
            persisted.pending,
            "the newer offer still awaits its own pass"
        );
        assert_eq!(persisted.offer_generation, state.offer_generation);
        assert_eq!(persisted.offered_at, unix_seconds(at(295)));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[cfg(unix)]
    #[test]
    fn an_unavailable_shared_file_applies_the_result_to_the_local_fallback() {
        use std::os::unix::fs::symlink;

        let l = layout("unavailable-result");
        install(&l, "claude", "2.1.280");
        let f = Vendors::default();
        f.head(CLAUDE_HEAD, "2.1.281", Some("\"c1\""));
        let mut watch = HeadWatch::shared(&[String::from("codex")]);
        assert_eq!(check(&mut watch, &l, &f, at(0)), ["claude"]);
        f.take_gets();
        let file = l.prefix.join("vendor-head-claude.watch");
        std::fs::remove_file(&file).unwrap();
        symlink("/dev/null", &file).unwrap();
        watch.note_pass_result("claude", false, at(295));
        assert!(!watch.watched[0].1.pending);
        assert_eq!(watch.watched[0].1.short_retries, 1);
        assert!(check(&mut watch, &l, &f, at(296)).is_empty());
        assert_eq!(check(&mut watch, &l, &f, at(595)), ["claude"]);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn a_wall_clock_rollback_refreshes_the_durable_check_stamp() {
        let l = layout("clock-rollback");
        install(&l, "claude", "2.1.280");
        let f = Vendors::default();
        f.head(CLAUDE_HEAD, "2.1.280", Some("\"c1\""));
        let mut watch = HeadWatch::shared(&[String::from("codex")]);
        let before = at(10_000);
        assert!(check(&mut watch, &l, &f, before).is_empty());
        let stamp = || {
            crate::vendor_direct::ProgramStamp::read(&l, "claude")
                .unwrap()
                .last_checked_at
        };
        assert_eq!(
            stamp(),
            i64::try_from(unix_seconds(before).unwrap()).unwrap()
        );
        let after = before - Duration::from_secs(2 * 3600);
        assert!(check(&mut watch, &l, &f, after).is_empty());
        assert_eq!(
            stamp(),
            i64::try_from(unix_seconds(after).unwrap()).unwrap()
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A pass that did not reach the offered head gets one five-minute retry. A
    /// persistently refused version then waits an hour, even across process changes.
    #[test]
    fn failed_targeted_pass_gets_one_short_retry_then_hourly_offers() {
        let l = layout("failed-pass-retry");
        install(&l, "claude", "2.1.280");
        let f = Vendors::default();
        f.head(CLAUDE_HEAD, "2.1.281", Some("\"c1\""));
        let excluded = [String::from("codex")];
        let mut first = HeadWatch::shared(&excluded);
        assert_eq!(check(&mut first, &l, &f, at(0)), ["claude"]);
        assert!(!first.head_reached(&l, "claude"));
        first.note_pass_result("claude", false, at(5));
        f.take_gets();
        let mut restarted = HeadWatch::shared(&excluded);
        assert!(check(&mut restarted, &l, &f, at(300)).is_empty());
        assert_eq!(check(&mut restarted, &l, &f, at(305)), ["claude"]);
        restarted.note_pass_result("claude", false, at(310));
        assert!(check(&mut restarted, &l, &f, at(610)).is_empty());
        assert_eq!(
            check(&mut restarted, &l, &f, at(3910)),
            ["claude"],
            "the next offer is an hour after the second failure"
        );
        install(&l, "claude", "2.1.281");
        assert!(restarted.head_reached(&l, "claude"));
        assert!(check(&mut restarted, &l, &f, at(3970)).is_empty());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Writing the offer precedes spawning the child. If that window exits in between,
    /// another window gets a bounded five-minute recovery instead of a one-hour silence.
    #[test]
    fn an_unacknowledged_offer_recovers_after_five_minutes() {
        let l = layout("unacknowledged-offer");
        install(&l, "claude", "2.1.280");
        let f = Vendors::default();
        f.head(CLAUDE_HEAD, "2.1.281", Some("\"c1\""));
        let excluded = [String::from("codex")];
        let mut crashed = HeadWatch::shared(&excluded);
        assert_eq!(check(&mut crashed, &l, &f, at(0)), ["claude"]);
        f.take_gets();
        drop(crashed);
        let mut successor = HeadWatch::shared(&excluded);
        assert!(check(&mut successor, &l, &f, at(299)).is_empty());
        assert_eq!(check(&mut successor, &l, &f, at(300)), ["claude"]);
        assert_eq!(
            f.take_gets(),
            [(CLAUDE_HEAD.to_string(), Some(String::from("\"c1\"")))],
            "the successor reuses the first process's validator"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// If a window repeatedly disappears before reporting its pass, the first two
    /// offers recover at five minutes, then the unchanged head falls back to hourly.
    #[test]
    fn an_unacknowledged_head_uses_two_short_leases_then_hourly_304s() {
        let l = layout("not-landed");
        install(&l, "claude", "2.1.280");
        let f = Vendors::default();
        f.head(CLAUDE_HEAD, "2.1.281", Some("\"c1\""));
        let mut w = HeadWatch::new(&[String::from("codex")]);
        assert_eq!(check(&mut w, &l, &f, at(0)), ["claude"]);
        w.take_notes();
        f.take_gets();
        let mut offered = Vec::new();
        for secs in (60..=2 * 3600).step_by(60) {
            if !check(&mut w, &l, &f, at(secs)).is_empty() {
                offered.push(secs);
            }
            assert_eq!(
                f.take_gets(),
                [(CLAUDE_HEAD.to_string(), Some(String::from("\"c1\"")))],
                "{secs}: one conditional GET"
            );
        }
        assert_eq!(offered, [300, 600, 4200], "bounded recovery, then hourly");
        assert_eq!(
            w.take_notes(),
            [
                "vendor head still ahead: claude 2.1.281 (installed 2.1.280)",
                "vendor head still ahead: claude 2.1.281 (installed 2.1.280)",
                "vendor head still ahead: claude 2.1.281 (installed 2.1.280)"
            ]
        );
        // Landed: the same 304s offer nothing.
        install(&l, "claude", "2.1.281");
        assert!(check(&mut w, &l, &f, at(3 * 3600)).is_empty());
        assert!(check(&mut w, &l, &f, at(4 * 3600)).is_empty());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The watch runs only where the pass it hints for reaches a vendor: the manager is
    /// on, and the registry is the network's, not a `dir:` mirror.
    #[test]
    fn the_watch_runs_only_where_the_pass_reaches_a_vendor() {
        let dir = OsStr::new("dir:/mirror");
        assert!(admitted(true, None));
        assert!(!admitted(false, None), "no pinned root");
        assert!(
            !admitted(true, Some(dir)),
            "a dir: registry reaches no vendor"
        );
        assert!(!admitted(false, Some(dir)));
        assert!(
            admitted(true, Some(OsStr::new("https://example.invalid"))),
            "anything but dir: is the network fetcher"
        );
        assert_eq!(
            crate::cli::dir_registry(Some(dir)),
            Some(PathBuf::from("/mirror")),
            "the pass's own choice"
        );
    }

    #[test]
    fn the_cadence_is_one_minute_on_the_wall_clock() {
        let l = layout("cadence");
        install(&l, "claude", "2.1.280");
        let f = Vendors::default();
        f.head(CLAUDE_HEAD, "2.1.280", Some("\"c1\""));
        let mut w = HeadWatch::new(&[]);
        check(&mut w, &l, &f, at(0));
        assert!(!w.due(at(59)));
        assert!(w.due(at(60)));
        // A check that is not due asks nothing.
        f.take_gets();
        assert!(check(&mut w, &l, &f, at(59)).is_empty());
        assert!(f.take_gets().is_empty());
        // A wall clock set back a day does not freeze the watch for a day.
        assert!(w.due(at(0) - Duration::from_secs(86_400)));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn a_wake_asks_every_head_twenty_seconds_later() {
        let l = layout("wake");
        install(&l, "claude", "2.1.280");
        let f = Vendors::default();
        f.head(CLAUDE_HEAD, "2.1.280", Some("\"c1\""));
        let mut w = HeadWatch::new(&[]);
        check(&mut w, &l, &f, at(0));
        // An ordinary slice, a small NTP step, and a clock set back are no wake.
        let five = Duration::from_secs(5);
        assert!(!w.note_slice(at(5), Some(five), five));
        assert!(!w.note_slice(at(40), Some(Duration::from_secs(34)), five));
        assert!(!w.note_slice(at(40), None, five));
        assert!(!w.due(at(55)), "still on the cadence");
        // Eight hours asleep: the wall clock moved, the monotonic clock did not.
        let woke = at(8 * 3600);
        assert!(w.note_slice(woke, Some(Duration::from_secs(8 * 3600)), five));
        assert!(!w.due(woke + Duration::from_secs(19)), "the network's 20 s");
        assert!(w.due(woke + AFTER_WAKE));
        assert!(check(&mut w, &l, &f, woke + AFTER_WAKE).is_empty());
        assert!(!w.due(woke + AFTER_WAKE + Duration::from_secs(59)));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn the_same_head_is_offered_hourly_after_the_short_retry_is_spent() {
        let l = layout("reoffer");
        install(&l, "claude", "2.1.280");
        let f = Vendors::default();
        // No ETag: every check reads the head in full.
        f.head(CLAUDE_HEAD, "2.1.281", None);
        let mut w = HeadWatch::new(&[]);
        assert_eq!(check(&mut w, &l, &f, at(0)), ["claude"]);
        w.note_pass_result("claude", false, at(0));
        assert_eq!(check(&mut w, &l, &f, at(300)), ["claude"]);
        w.note_pass_result("claude", false, at(300));
        for secs in [600, 3300] {
            assert!(check(&mut w, &l, &f, at(secs)).is_empty(), "{secs}");
        }
        assert_eq!(check(&mut w, &l, &f, at(3900)), ["claude"], "an hour on");
        // A head that differs from the one offered is offered at once.
        f.head(CLAUDE_HEAD, "2.1.282", None);
        assert_eq!(check(&mut w, &l, &f, at(4200)), ["claude"]);
        install(&l, "claude", "2.1.282");
        w.note_pass_result("claude", true, at(4200));
        assert!(check(&mut w, &l, &f, at(4500)).is_empty());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn an_unreachable_head_is_asked_every_minute_and_logged_once() {
        let l = layout("unreachable");
        install(&l, "claude", "2.1.280");
        let f = Vendors::default();
        f.answer(CLAUDE_HEAD, Answer::Unreachable);
        // An excluded program is never scheduled, so `due` below is claude's alone.
        let mut w = HeadWatch::new(&[String::from("codex")]);
        assert!(check(&mut w, &l, &f, at(0)).is_empty());
        assert_eq!(
            w.take_notes(),
            ["vendor head unreachable: claude (timed out)"]
        );
        assert!(!w.due(at(59)));
        assert!(w.due(at(60)), "retried in a minute");
        assert!(check(&mut w, &l, &f, at(60)).is_empty());
        assert!(w.take_notes().is_empty(), "said once");
        assert!(!w.due(at(60 + 59)));
        assert!(w.due(at(60 + 60)), "then every minute");
        assert!(check(&mut w, &l, &f, at(120)).is_empty());
        assert!(w.take_notes().is_empty());
        // Back: said once, and a moved head triggers.
        f.head(CLAUDE_HEAD, "2.1.281", Some("\"c1\""));
        assert_eq!(check(&mut w, &l, &f, at(180)), ["claude"]);
        assert_eq!(
            w.take_notes(),
            [
                "vendor head reachable again: claude",
                "vendor head moved: claude 2.1.281 (installed 2.1.280)"
            ]
        );
        assert!(!w.due(at(180 + 59)));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn a_refused_head_is_no_trigger_and_is_asked_at_the_cadence() {
        let l = layout("refused");
        install(&l, "claude", "2.1.280");
        install(&l, "codex", "0.156.0");
        let f = Vendors::default();
        // Not canonical; a tag that is not codex's; a head the host does not serve.
        f.head(CLAUDE_HEAD, "2.1.0281", None);
        f.head(CODEX_HEAD, "{\"tag_name\":\"v9.9.9\"}", None);
        let mut w = HeadWatch::new(&[]);
        assert!(check(&mut w, &l, &f, at(0)).is_empty());
        f.answer(CLAUDE_HEAD, Answer::Unserved);
        f.head(
            CODEX_HEAD,
            "{\"tag_name\":\"rust-v0.157.0\",\"tag_name\":\"rust-v0.157.0\"}",
            None,
        );
        assert!(!w.due(at(59)));
        assert!(check(&mut w, &l, &f, at(60)).is_empty());
        assert!(w.take_notes().is_empty(), "nothing to log");
        assert!(
            !w.due(at(119)),
            "the normal cadence, not the unreachable retry"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn a_legacy_or_unrecorded_build_is_older_than_any_head() {
        let l = layout("legacy");
        activate(&l, "claude", 2_026_092_201);
        // A vendor build whose record never landed (`.ready` missing).
        activate(&l, "codex", v("0.157.0").build_id());
        let f = Vendors::default();
        f.head(CLAUDE_HEAD, "2.1.280", None);
        f.codex("0.156.0", None);
        let mut w = HeadWatch::new(&[]);
        assert_eq!(check(&mut w, &l, &f, at(0)), ["claude", "codex"]);
        assert_eq!(
            w.take_notes(),
            [
                "vendor head moved: claude 2.1.280 (installed version unknown)",
                "vendor head moved: codex 0.156.0 (installed version unknown)"
            ]
        );
        // Once the lane has read the legacy build's version, a head at it is no trigger;
        // one past it is.
        crate::vendor_direct::ProgramStamp::record_legacy_version(
            &l,
            "claude",
            2_026_092_201,
            v("2.1.280"),
        )
        .unwrap();
        let mut w = HeadWatch::new(&[String::from("codex")]);
        assert!(check(&mut w, &l, &f, at(0)).is_empty());
        f.head(CLAUDE_HEAD, "2.1.281", None);
        assert_eq!(check(&mut w, &l, &f, at(300)), ["claude"]);
        assert_eq!(
            w.take_notes(),
            ["vendor head moved: claude 2.1.281 (installed 2.1.280)"]
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn a_program_the_watch_leaves_alone_costs_no_request() {
        let f = Vendors::default();
        f.head(CLAUDE_HEAD, "2.1.281", None);
        f.codex("0.157.0", None);
        // Not installed at all.
        let l = layout("skip-absent");
        let mut w = HeadWatch::new(&[]);
        assert!(check(&mut w, &l, &f, at(0)).is_empty());
        assert!(f.take_gets().is_empty());
        let _ = std::fs::remove_dir_all(&l.prefix);
        // Installed, then excluded, removed, or held.
        let l = layout("skip");
        install(&l, "claude", "2.1.280");
        install(&l, "codex", "0.156.0");
        let mut w = HeadWatch::new(&[String::from("claude")]);
        std::fs::write(l.removed(), "# removed\ncodex\n").unwrap();
        assert!(check(&mut w, &l, &f, at(0)).is_empty());
        assert!(f.take_gets().is_empty(), "excluded and removed");
        std::fs::remove_file(l.removed()).unwrap();
        crate::pin::set_pinned(&l, "codex", true).unwrap();
        assert!(check(&mut w, &l, &f, at(300)).is_empty());
        assert!(f.take_gets().is_empty(), "held by a local pin");
        crate::pin::set_pinned(&l, "codex", false).unwrap();
        assert_eq!(
            check(&mut w, &l, &f, at(600)),
            ["codex"],
            "the negative control"
        );
        assert_eq!(f.take_gets(), [(CODEX_HEAD.to_string(), None)]);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// `[packages].exclude` IS READ LIVE (§4.3's owed item): a program excluded after the
    /// watch started is neither due nor asked from the next slice, and taking it off the
    /// list asks it at once — a stale launch-time list neither kept hinting passes for a
    /// program the person excluded nor stranded one they included again.
    #[test]
    fn an_exclusion_edited_mid_watch_is_heard_at_the_next_check() {
        let l = layout("live-exclude");
        install(&l, "claude", "2.1.280");
        let f = Vendors::default();
        f.head(CLAUDE_HEAD, "2.1.281", None);
        let codex = [String::from("codex")];
        let both = [String::from("claude"), String::from("codex")];
        let mut w = HeadWatch::new(&codex);
        w.set_exclude(&both);
        assert!(!w.due(at(0)), "every watched head is excluded");
        assert!(check(&mut w, &l, &f, at(0)).is_empty());
        assert!(f.take_gets().is_empty(), "excluded: no request");
        w.set_exclude(&codex);
        assert!(w.due(at(5)), "included again: due at once");
        assert_eq!(check(&mut w, &l, &f, at(5)), ["claude"]);
        assert_eq!(f.take_gets(), [(CLAUDE_HEAD.to_string(), None)]);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// ONE LIVE CHECK against the real vendors (network; run by hand with `--ignored`):
    /// builds older than the heads measured on 2026-09-22 trigger both programs; builds
    /// at those heads trigger nothing, and the next check is two 304s.
    #[test]
    #[ignore = "reaches the real vendor hosts"]
    fn live_heads_against_the_real_vendors() {
        let answers: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let get = |p: &str, u: &str, c: u64, e: Option<&str>| {
            let got = network_get(p, u, c, e);
            answers.borrow_mut().push(match &got {
                Ok(VendorGet::NotModified { etag }) => format!("{p}: 304 ({etag})"),
                Ok(VendorGet::Body { bytes, etag, .. }) => format!(
                    "{p}: 200, {} bytes, etag {etag:?}, head {:?}",
                    bytes.len(),
                    String::from_utf8_lossy(&bytes[..bytes.len().min(40)])
                ),
                Err(e) => format!("{p}: {e:?}"),
            });
            got
        };
        let l = layout("live-older");
        install(&l, "claude", "2.1.279");
        install(&l, "codex", "0.155.0");
        let mut w = HeadWatch::new(&[]);
        let moved = w.check(&l, SystemTime::now(), &get);
        eprintln!("older builds → triggered {moved:?}");
        for line in answers.borrow_mut().drain(..).chain(w.take_notes()) {
            eprintln!("  {line}");
        }
        assert_eq!(moved, ["claude", "codex"]);
        let _ = std::fs::remove_dir_all(&l.prefix);

        let l = layout("live-current");
        install(&l, "claude", "2.1.280");
        install(&l, "codex", "0.156.0");
        let mut w = HeadWatch::new(&[]);
        let now = SystemTime::now();
        let moved = w.check(&l, now, &get);
        eprintln!("builds at the heads → triggered {moved:?}");
        for line in answers.borrow_mut().drain(..).chain(w.take_notes()) {
            eprintln!("  {line}");
        }
        assert!(moved.is_empty());
        let moved = w.check(&l, now + CADENCE, &get);
        let second: Vec<String> = answers.borrow_mut().drain(..).collect();
        eprintln!("one minute on → triggered {moved:?}");
        for line in &second {
            eprintln!("  {line}");
        }
        assert!(moved.is_empty());
        assert!(
            second.iter().all(|a| a.contains(": 304 (")),
            "both heads answer 304 to their ETags: {second:?}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}
