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
        crate::cli::registry_seam().as_deref(),
    )
    .then(|| {
        (
            HeadWatch::hosted(exclude, host),
            Arc::new(network_get) as Arc<ConcurrentVendorGetFn<'static>>,
        )
    })
}

fn admitted(manager_enabled: bool, registry: Option<&OsStr>) -> bool {
    manager_enabled && crate::cli::dir_registry(registry).is_none()
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
    if crate::alab::build_is_alab(layout, program, build) {
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

    /// The hang detector for a wait that must end (a worker's answer, a test's own
    /// release).
    pub(super) const HANG: Duration = Duration::from_secs(60);

    #[test]
    fn an_independent_package_hint_interrupts_an_idle_vendor_park() {
        let mut watch = HeadWatch::new(&[]);
        let owner = thread::current();
        let notifier = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            owner.unpark();
        });
        let started = std::time::Instant::now();
        watch.park_for_hint(2 * HANG);
        assert!(started.elapsed() < HANG, "{:?}", started.elapsed());
        notifier.join().unwrap();
    }

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000 + secs)
    }

    pub(super) fn layout(label: &str) -> crate::testdir::ScratchLayout {
        crate::testdir::layout(&format!("atpkg-watch-{label}-"))
    }

    #[test]
    fn a_window_sit_waits_for_a_sessions_held_move_look() {
        let l = layout("sit-handoff");
        let mut session = host::HostClaim::new(Host::Session, &l);
        assert_eq!(session.admit().0, host::Admit::Yes);
        assert!(session.seated_session());
        let mut window = HeadWatch::hosted(&[], Host::Window);
        assert!(!window.sit(&l), "the session owns this slice's look");
        assert_eq!(
            host::watcher(&l),
            host::Watcher::Session(Some(std::process::id()))
        );
        session.end_round();
        assert!(window.sit(&l), "the next slice takes the shared claim");
        assert_eq!(host::watcher(&l), host::Watcher::Window);
        assert_eq!(session.admit().0, host::Admit::Later(CADENCE));
        drop(window);
        drop(session);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    fn activate(layout: &Layout, program: &str, build: u64) -> PathBuf {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        crate::activate::atomic_symlink(&dir, &layout.program_current(program)).unwrap();
        dir
    }

    /// A complete, recorded vendor build of `program` at `version`, active.
    pub(super) fn install(layout: &Layout, program: &str, version: &str) {
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
            Anchor::GoogleTwoHost => format!(
                "https://storage.googleapis.com/antigravity-public/antigravity-cli/{version}-1/darwin-arm/cli_mac_arm64.tar.gz"
            ),
            Anchor::MetaDeveloperId => format!(
                "https://lookaside.facebook.com/lookaside/muse/download/?channel=muse&version={version}&file=muse-aarch64-macos"
            ),
            Anchor::MoonshotTwoHost => {
                format!("https://cdn.kimi.com/kimi-code/binaries/{version}/kimi-code-darwin-arm64")
            }
            Anchor::XaiNpmRegistry | Anchor::XaiDeveloperId => format!(
                "https://registry.npmjs.org/@xai-official/grok-darwin-arm64/-/grok-darwin-arm64-{version}.tgz"
            ),
        };
        let record = VendorRecord {
            schema: RECORD_SCHEMA,
            program: program.to_string(),
            version,
            vendor: spec.vendor.to_string(),
            source_url,
            sha256: "a".repeat(64),
            sha512: String::new(),
            size: 1,
            tree_root: "b".repeat(64),
            apple_team: cfg!(target_os = "macos").then(|| spec.apple_team.to_string()),
            anchor: spec.anchor,
            build_date: None,
            verified_at: 1_790_000_000,
            measured_sha256: None,
            agreed_with: None,
            tree_from: None,
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
        assert_eq!(w.take_notes().len(), 1);
        assert_eq!(
            f.take_gets(),
            [
                (CLAUDE_HEAD.to_string(), None),
                (CODEX_HEAD.to_string(), None)
            ]
        );
        let mut w = HeadWatch::new(&[]);
        f.head(CLAUDE_HEAD, "2.1.279", None);
        f.codex("0.155.0", None);
        assert!(check(&mut w, &l, &f, at(0)).is_empty());
        assert!(w.take_notes().is_empty());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn a_stale_completion_never_cuts_a_park_short() {
        let mut watch = HeadWatch::new(&[]);
        watch.completed_tx.send(0).unwrap();
        watch.park_for_hint(Duration::ZERO);
        assert!(
            watch.ready_notices.is_empty(),
            "the stale notice was dropped"
        );
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
                        .wait_timeout_while(released, 2 * HANG, |released| !*released)
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
        let deadline = std::time::Instant::now() + HANG;
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
        watch.park_for_hint(HANG);
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
                .wait_timeout_while(released, 2 * HANG, |released| !*released)
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
        started_rx.recv_timeout(HANG).unwrap();
        let early = returned_rx.recv_timeout(HANG);
        let returned_while_blocked = early.is_ok();
        let owned_while_blocked = early
            .as_ref()
            .is_ok_and(|(watch, moved)| moved.is_empty() && watch.pending_count() == 1);
        let (lock, ready) = &*gate;
        *lock.lock().unwrap() = true;
        ready.notify_all();
        let (mut watch, moved) = early.unwrap_or_else(|_| returned_rx.recv_timeout(HANG).unwrap());
        caller.join().unwrap();
        assert!(
            returned_while_blocked,
            "one slow vendor GET blocked the package lane"
        );
        assert!(owned_while_blocked, "the blocked GET must remain owned");
        assert!(moved.is_empty());
        watch.park_for_hint(HANG);
        assert_eq!(watch.check_pending(&l, at(0), &get), ["claude"]);
        assert_eq!(watch.pending_count(), 0);
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
        let model = aterm_models_atpkg::atpkg_index_pending_park_model();
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

        assert!(
            watch
                .check_pending_with_index_in_flight(&l, at(0) + BUSY_RETRY, &get, true)
                .is_empty()
        );
        assert_eq!(watch.pending_count(), 1);
        watch.park_for_hint(HANG);
        assert_eq!(
            watch.check_pending_with_index_in_flight(&l, at(0) + BUSY_RETRY, &get, true),
            ["claude"]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(model.fire("VendorReady", &mut state));
        assert_eq!(watch.pending_count(), 0);

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
        let model = aterm_models_atpkg::atpkg_index_pending_park_model();
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
        watch.park_for_hint(HANG);
        assert!(watch.check_pending(&l, at(0), &get).is_empty());
        assert_eq!(watch.pending_count(), 0);
        assert!(!watch.round_active);
        assert_eq!(watch.watched[0].1.next_check_at, Some(at(0) + BUSY_RETRY));
        assert!(!watch.due(at(0) + BUSY_RETRY - Duration::from_millis(1)));
        assert!(watch.due(at(0) + BUSY_RETRY));

        assert!(watch.check_pending(&l, at(0) + BUSY_RETRY, &get).is_empty());
        assert_eq!(watch.pending_count(), 1);
        watch.park_for_hint(HANG);
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
                    .wait_timeout_while(released, 2 * HANG, |released| !*released)
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
        codex_started_rx.recv_timeout(HANG).unwrap();
        watch.park_for_hint(HANG);
        install(&l, "codex", "0.157.0");
        assert!(
            watch.check_pending(&l, at(0), &get).is_empty(),
            "the whole pass reached Codex before the hint was harvested"
        );
        assert_eq!(watch.pending_count(), 1);
        let (lock, ready) = &*release_claude;
        *lock.lock().unwrap() = true;
        ready.notify_all();
        watch.park_for_hint(HANG);
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
        assert_eq!(w.take_notes().len(), 3);
        install(&l, "claude", "2.1.281");
        assert!(check(&mut w, &l, &f, at(3 * 3600)).is_empty());
        assert!(check(&mut w, &l, &f, at(4 * 3600)).is_empty());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

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
        f.take_gets();
        assert!(check(&mut w, &l, &f, at(59)).is_empty());
        assert!(f.take_gets().is_empty());
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
        let five = Duration::from_secs(5);
        assert!(!w.note_slice(at(5), Some(five), five));
        assert!(!w.note_slice(at(40), Some(Duration::from_secs(34)), five));
        assert!(!w.note_slice(at(40), None, five));
        assert!(!w.due(at(55)), "still on the cadence");
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
        let mut w = HeadWatch::new(&[String::from("codex")]);
        assert!(check(&mut w, &l, &f, at(0)).is_empty());
        assert_eq!(w.take_notes().len(), 1);
        assert!(!w.due(at(59)));
        assert!(w.due(at(60)), "retried in a minute");
        assert!(check(&mut w, &l, &f, at(60)).is_empty());
        assert!(w.take_notes().is_empty(), "said once");
        assert!(!w.due(at(60 + 59)));
        assert!(w.due(at(60 + 60)), "then every minute");
        assert!(check(&mut w, &l, &f, at(120)).is_empty());
        assert!(w.take_notes().is_empty());
        f.head(CLAUDE_HEAD, "2.1.281", Some("\"c1\""));
        assert_eq!(check(&mut w, &l, &f, at(180)), ["claude"]);
        assert_eq!(w.take_notes().len(), 2);
        assert!(!w.due(at(180 + 59)));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn a_refused_head_is_no_trigger_and_is_asked_at_the_cadence() {
        let l = layout("refused");
        install(&l, "claude", "2.1.280");
        install(&l, "codex", "0.156.0");
        let f = Vendors::default();
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
        activate(&l, "codex", v("0.157.0").build_id());
        let f = Vendors::default();
        f.head(CLAUDE_HEAD, "2.1.280", None);
        f.codex("0.156.0", None);
        let mut w = HeadWatch::new(&[]);
        assert_eq!(check(&mut w, &l, &f, at(0)), ["claude", "codex"]);
        assert_eq!(w.take_notes().len(), 2);
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
        assert_eq!(w.take_notes().len(), 1);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn a_program_the_watch_leaves_alone_costs_no_request() {
        let f = Vendors::default();
        f.head(CLAUDE_HEAD, "2.1.281", None);
        f.codex("0.157.0", None);
        let l = layout("skip-absent");
        let mut w = HeadWatch::new(&[]);
        assert!(check(&mut w, &l, &f, at(0)).is_empty());
        assert!(f.take_gets().is_empty());
        let _ = std::fs::remove_dir_all(&l.prefix);
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

    #[test]
    fn an_exclusion_edited_mid_watch_is_heard_at_the_next_check() {
        let l = layout("live-exclude");
        install(&l, "claude", "2.1.280");
        let f = Vendors::default();
        f.head(CLAUDE_HEAD, "2.1.281", None);
        let all: Vec<String> = VENDORS.iter().map(|s| s.program.to_string()).collect();
        let others: Vec<String> = all.iter().filter(|p| *p != "claude").cloned().collect();
        let mut w = HeadWatch::new(&others);
        w.set_exclude(&all);
        assert!(!w.due(at(0)), "every watched head is excluded");
        assert!(check(&mut w, &l, &f, at(0)).is_empty());
        assert!(f.take_gets().is_empty(), "excluded: no request");
        w.set_exclude(&others);
        assert!(w.due(at(5)), "included again: due at once");
        assert_eq!(check(&mut w, &l, &f, at(5)), ["claude"]);
        assert_eq!(f.take_gets(), [(CLAUDE_HEAD.to_string(), None)]);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}
