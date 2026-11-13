// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The vendor head watch (design §1.6).

use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read as _, Seek as _, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

mod host;
mod runner;

pub use host::{Host, Watcher, watcher};
pub use runner::{
    Clock, DetachedPasses, Gate, HostHandle, LiveSettings, PassLauncher, Runner, RunningPass,
    SLICE, Settings, SystemClock, run_host,
};

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
/// A head already offered is offered again only after this long.
pub const REOFFER_AFTER: Duration = Duration::from_secs(60 * 60);
/// One quick retry after a head-watch pass left the installed version behind.
pub const FAILED_PASS_RETRY: Duration = Duration::from_secs(5 * 60);
const MAX_SHORT_RETRIES: u8 = 1;
const BUSY_RETRY: Duration = Duration::from_secs(5);
const SHARED_SCHEMA: u32 = 2;
const MAX_SHARED_BYTES: u64 = 1024;
const DURABLE_CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// The wall clock outrunning the monotonic clock by more than this over one slice is a wake.
pub const WAKE_GAP: Duration = Duration::from_secs(30);

/// One small, store-scoped file per vendor, also serving as its flock inode. The file is
/// only a wake hint: neither its version nor its validator can authorize an install.
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
    fn due(&self, now: SystemTime) -> bool {
        self.next_check_at
            .is_none_or(|at| at <= now || at > now + CADENCE + WAKE_GAP)
    }

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

    const fn apply_result(&mut self, result: DeferredResult) {
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
    /// The current park owner, if any. A watch may move between threads before
    /// its owned GET finishes, so workers look up the parker at completion.
    parker: Arc<Mutex<Option<thread::Thread>>>,
    ready_notices: Vec<usize>,
    /// A completed hint whose park was preempted by a bump, full-pass deadline,
    /// or index answer.
    deferred_moved: Vec<&'static str>,
    round_notes: Vec<Option<Vec<String>>>,
    round_active: bool,
    /// The host that runs this watch — `None` for a watch no host runs (a test's), which
    /// is always admitted.
    host: Option<Host>,
    /// That host's claim on the store's rendezvous ([`host`]), made at the first round
    /// (the store is known then) and kept: a window's for good, a session's seat for good
    /// and its rendezvous for one round.
    claim: Option<host::HostClaim>,
    #[cfg(test)]
    fail_next_pending_spawn: bool,
}

impl HeadWatch {
    /// A watch with every head due now, over every vendor-direct program but those `exclude` names.
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
            parker: Arc::new(Mutex::new(None)),
            ready_notices: Vec::new(),
            deferred_moved: Vec::new(),
            round_notes,
            round_active: false,
            host: None,
            claim: None,
            #[cfg(test)]
            fail_next_pending_spawn: false,
        }
    }

    /// `[packages].exclude` as it reads now.
    pub fn set_exclude(&mut self, exclude: &[String]) {
        if self.exclude != exclude {
            self.exclude = exclude.to_vec();
        }
    }

    fn excludes(exclude: &[String], program: &str) -> bool {
        exclude.iter().any(|p| p == program)
    }

    fn shared(exclude: &[String]) -> Self {
        let mut watch = Self::new(exclude);
        watch.shared = true;
        watch
    }

    /// The watch `host` runs.
    #[must_use]
    pub fn hosted(exclude: &[String], host: Host) -> Self {
        let mut watch = Self::shared(exclude);
        watch.host = Some(host);
        watch
    }

    fn admitted(&mut self, layout: &Layout, now: SystemTime) -> bool {
        let Some(kind) = self.host else {
            return true;
        };
        let claim = self
            .claim
            .get_or_insert_with(|| host::HostClaim::new(kind, layout));
        let (admit, note) = claim.admit();
        self.notes.extend(note);
        match admit {
            host::Admit::Yes => true,
            host::Admit::Later(after) => {
                for (_, watched) in &mut self.watched {
                    if watched.due(now) {
                        watched.next_check_at = Some(now + after);
                    }
                }
                false
            }
        }
    }

    /// Let the host's claim go — a session whose Automatic updates were turned off.
    pub fn release_host(&mut self) {
        if !self.has_pending() {
            self.claim = None;
        }
    }

    /// Whether this watch's terminal session holds the store's exclusive rendezvous right now.
    #[must_use]
    pub fn seated_session(&self) -> bool {
        self.claim
            .as_ref()
            .is_some_and(host::HostClaim::seated_session)
    }

    fn session_behind_window(&self) -> bool {
        self.claim
            .as_ref()
            .is_some_and(host::HostClaim::window_watches)
    }

    #[cfg(test)]
    fn session_admission_attempts(&self) -> u64 {
        self.claim.as_ref().map_or(0, host::HostClaim::admit_calls)
    }

    fn begin_session_update_look(&mut self, layout: &Layout, now: SystemTime) -> bool {
        self.host == Some(Host::Session)
            && layout.prefix.is_dir()
            && self.admitted(layout, now)
            && self.seated_session()
    }

    fn end_session_update_look(&mut self) {
        if self.host == Some(Host::Session)
            && !self.has_pending()
            && let Some(claim) = &mut self.claim
        {
            claim.end_round();
        }
    }

    /// Establish a window's persistent shared claim even with no vendor head to check.
    #[must_use]
    pub fn sit(&mut self, layout: &Layout) -> bool {
        if self.host != Some(Host::Window) || !layout.prefix.is_dir() {
            return false;
        }
        if self.round_active || self.has_pending() {
            return true;
        }
        let claim = self
            .claim
            .get_or_insert_with(|| host::HostClaim::new(Host::Window, layout));
        let (admit, note) = claim.admit();
        self.notes.extend(note);
        admit == host::Admit::Yes
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

    /// Keep a completed offer when a higher-priority package event wins this park.
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

    /// A check already launched but not yet harvested.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.iter().filter(|slot| slot.is_some()).count()
    }

    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.pending_count() != 0
    }

    /// Park until a vendor worker wakes this thread, or the ordinary package-loop slice elapses.
    pub fn park_for_hint(&mut self, slice: Duration) {
        if !self.deferred_moved.is_empty() {
            return;
        }
        *self.parker.lock().unwrap_or_else(|p| p.into_inner()) = Some(thread::current());
        self.take_notices();
        if self.ready_notices.is_empty() {
            thread::park_timeout(slice);
        }
        *self.parker.lock().unwrap_or_else(|p| p.into_inner()) = None;
        if self.take_notices() || !self.ready_notices.is_empty() {
            thread::park_timeout(Duration::ZERO);
        }
    }

    fn take_notices(&mut self) -> bool {
        let mut arrived = false;
        while let Ok(index) = self.completed_rx.try_recv() {
            arrived = true;
            let live = self.pending.get(index).is_some_and(Option::is_some);
            if live && !self.ready_notices.contains(&index) {
                self.ready_notices.push(index);
            }
        }
        arrived
    }

    /// One slice of the caller's park, over which the wall clock moved `wall` (`None`: backwards) and the monotonic clock `mono`.
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

    /// One slice of a host's park (design §1.6), the unit both hosts run.
    pub fn slice(
        &mut self,
        layout: &Layout,
        wall: SystemTime,
        wall_elapsed: Option<Duration>,
        mono_elapsed: Duration,
        get: &Arc<ConcurrentVendorGetFn<'static>>,
        index_in_flight: bool,
    ) -> Option<Vec<&'static str>> {
        self.note_slice(wall, wall_elapsed, mono_elapsed);
        if !self.due(wall) {
            return None;
        }
        let moved = self.check_pending_with_index_in_flight(layout, wall, get, index_in_flight);
        (!moved.is_empty()).then_some(moved)
    }

    /// Ask every due head through `get`.
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
    pub fn check_pending(
        &mut self,
        layout: &Layout,
        now: SystemTime,
        get: &Arc<ConcurrentVendorGetFn<'static>>,
    ) -> Vec<&'static str> {
        self.check_pending_with_index_in_flight(layout, now, get, false)
    }

    /// [`Self::check_pending`]; `_index_in_flight` is ignored.
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
        let mut due: Vec<usize> = Vec::new();
        for (index, (spec, watched)) in self.watched.iter_mut().enumerate() {
            if Self::excludes(&self.exclude, spec.program) || !watched.due(now) {
                continue;
            }
            if installed(layout, spec).is_some() {
                due.push(index);
            } else {
                watched.next_check_at = Some(now + CADENCE);
            }
        }
        due.truncate(2);
        if due.is_empty() {
            return Vec::new();
        }
        if !self.admitted(layout, now) {
            return Vec::new();
        }
        self.round_active = true;
        let shared = self.shared;
        for index in due {
            let spec = self.watched[index].0;
            let mut snapshot = self.watched[index].1.clone();
            let layout_copy = layout.clone();
            let get_copy = Arc::clone(get);
            let completed_tx = self.completed_tx.clone();
            let parker = Arc::clone(&self.parker);
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
                        if let Some(thread) =
                            parker.lock().unwrap_or_else(|p| p.into_inner()).as_ref()
                        {
                            thread.unpark();
                        }
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
        if let Some(claim) = &mut self.claim {
            claim.end_round();
        }
    }

    /// A targeted pass which did not complete may retry the same head once after five minutes.
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

    /// A targeted pass this watch's offer started has ended.
    pub fn pass_ended(&mut self, layout: &Layout, program: &str, ran_ok: bool, now: SystemTime) {
        let reached = ran_ok && self.head_reached(layout, program);
        self.note_pass_result(program, reached, now);
    }

    /// Whether this store actually reached the version the watch offered.
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

    /// One log line each since the last call.
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
    let Some(file) = crate::index_probe::open_private(&path, true) else {
        return SharedFile::Unavailable;
    };
    match crate::lock::Flock::try_lock(file) {
        Ok(file) => SharedFile::Held(file),
        Err(std::fs::TryLockError::WouldBlock) => SharedFile::Busy,
        Err(std::fs::TryLockError::Error(_)) => SharedFile::Unavailable,
    }
}

fn read_shared(file: &mut File, spec: &VendorSpec) -> SharedState {
    let empty = || SharedState::empty(spec);
    let Ok(meta) = file.metadata() else {
        return empty();
    };
    if meta.len() > MAX_SHARED_BYTES {
        return empty();
    }
    let mut text =
        String::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
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
        state.last_record_at = Some(checked_at);
    }
    state.remember(&shared_watched);
    let _ = write_shared(&mut file, &state);
    *watched = shared_watched;
    outcome
}

/// The watch the window's package thread may run ([`for_host`] as [`Host::Window`]).
#[must_use]
pub fn for_this_process(
    exclude: &[String],
) -> Option<(HeadWatch, Arc<ConcurrentVendorGetFn<'static>>)> {
    for_host(Host::Window, exclude)
}

/// The watch `host` may run, with the GET it reads heads through.
#[must_use]
pub fn for_host(
    host: Host,
    exclude: &[String],
) -> Option<(HeadWatch, Arc<ConcurrentVendorGetFn<'static>>)> {
    admitted(
        crate::manager_enabled(),
        crate::source::registry_seam().as_deref(),
    )
    .then(|| {
        (
            HeadWatch::hosted(exclude, host),
            Arc::new(network_get) as Arc<ConcurrentVendorGetFn<'static>>,
        )
    })
}

fn admitted(manager_enabled: bool, registry: Option<&OsStr>) -> bool {
    manager_enabled && crate::source::dir_registry(registry).is_none()
}

/// The production GET.
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

/// Whether `layout`'s copy of `spec`'s program is one the watch keeps at its vendor's head.
pub(crate) fn follows_vendor(layout: &Layout, spec: &VendorSpec) -> bool {
    installed(layout, spec).is_some()
}

fn installed(layout: &Layout, spec: &VendorSpec) -> Option<Installed> {
    let program = spec.program;
    if layout.removed_programs().ok()?.contains(program)
        || crate::pin::holds_update(layout, program)
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
    if crate::alab::build_is_alab(layout, program, build)
        && !crate::alab::vendor_chosen(layout, program)
    {
        return None;
    }
    Some(if is_vendor_build(build) {
        super::complete_record(&dir).map_or(Installed::Unknown, |r| Installed::Version(r.version))
    } else {
        super::ProgramStamp::read(layout, program)
            .and_then(|s| s.legacy_version_of(build))
            .map_or(Installed::Unknown, Installed::Version)
    })
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
