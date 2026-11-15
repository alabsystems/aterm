// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit private-lab restore measurement. No release build admits this seam.

use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use aterm_uds::private_file::{PrivateDirectory, PrivateFileError};

const HANG: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(20);
const PENDING: u8 = 0;
const RELEASED: u8 = 1;
const FAILED: u8 = 2;
const MAX_POSTS: usize = 5000;
const MAX_EVENTS: usize = 32;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    start_file: String,
    events_file: String,
    #[serde(default)]
    wait_uri: Option<String>,
    #[serde(default)]
    release_file: Option<String>,
    #[serde(default)]
    closed_reopen: bool,
    #[serde(default)]
    journal_release_file: Option<String>,
    #[serde(default)]
    seed_closed_reopen: bool,
    #[serde(default)]
    seed_closed_view: bool,
}

impl Config {
    fn validate(&self) -> Result<(), String> {
        let leaf = |name: &str| {
            let mut parts = Path::new(name).components();
            matches!(parts.next(), Some(Component::Normal(_))) && parts.next().is_none()
        };
        if !leaf(&self.start_file)
            || !leaf(&self.events_file)
            || self.start_file == self.events_file
        {
            return Err("measurement start/events must be distinct filenames".into());
        }
        if self.seed_closed_view && !self.seed_closed_reopen {
            return Err("closed view fixture needs closed reopen setup".into());
        }
        if self.seed_closed_reopen && (!self.closed_reopen || self.wait_uri.is_none()) {
            return Err("closed reopen fixture needs its document URI and admission gates".into());
        }
        if let Some(journal) = &self.journal_release_file
            && (!self.closed_reopen
                || !leaf(journal)
                || journal == &self.start_file
                || journal == &self.events_file
                || self.release_file.as_ref() == Some(journal)
                || self.wait_uri.is_none())
        {
            return Err(
                "journal wait needs a closed reopen and a distinct release filename".into(),
            );
        }
        match (&self.wait_uri, &self.release_file) {
            (None, None) => Ok(()),
            (Some(uri), Some(release))
                if leaf(release)
                    && release != &self.start_file
                    && release != &self.events_file
                    && uri.len() <= 16 * 1024
                    && crate::native_document_host::file_uri_path(uri).is_ok() =>
            {
                Ok(())
            }
            _ => Err(
                "measurement file wait needs one file URI and a distinct release filename".into(),
            ),
        }
    }
}

#[derive(serde::Serialize)]
struct Event {
    event: &'static str,
    monotonic_ns: u64,
    pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    burst_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    post_ns: Option<Vec<u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    post_end_ns: Option<Vec<u64>>,
}

struct Measurement {
    config: Config,
    directory: PrivateDirectory,
    gate: AtomicU8,
    failed: AtomicBool,
    wait_taken: AtomicBool,
    journal_wait_taken: AtomicBool,
    fixture_taken: AtomicBool,
    fixture_candidate: Mutex<Option<(bool, crate::closed_recovery::RecoveryToken)>>,
    fixture_retained: AtomicBool,
    fixture_custody: AtomicU64,
    fixture_operations: AtomicU64,
    events: Mutex<Vec<Event>>,
    callbacks: CallbackStats,
    keys: KeyStats,
}

enum Loaded {
    Absent,
    Refused,
    Ready(Arc<Measurement>),
}

static LOADED: OnceLock<Loaded> = OnceLock::new();

#[derive(Default)]
struct CallbackStats {
    active: AtomicU64,
    count: AtomicU64,
    first: AtomicU64,
    last: AtomicU64,
    max: AtomicU64,
    max_begin: AtomicU64,
    max_end: AtomicU64,
    total: AtomicU64,
    finished: AtomicBool,
}

impl CallbackStats {
    fn record(&self, begin: u64, end: u64, finished: bool) {
        let _ = self
            .first
            .compare_exchange(0, begin, Ordering::Relaxed, Ordering::Relaxed);
        let elapsed = end.saturating_sub(begin);
        if elapsed > self.max.load(Ordering::Relaxed) {
            self.max_begin.store(begin, Ordering::Relaxed);
            self.max_end.store(end, Ordering::Relaxed);
            self.max.store(elapsed, Ordering::Release);
        }
        self.last.store(end, Ordering::Relaxed);
        self.total.fetch_add(elapsed, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Release);
        if finished {
            self.finished.store(true, Ordering::Release);
        }
    }

    fn reset(&self) {
        for counter in [
            &self.count,
            &self.first,
            &self.last,
            &self.max,
            &self.max_begin,
            &self.max_end,
            &self.total,
        ] {
            counter.store(0, Ordering::Relaxed);
        }
        self.finished.store(false, Ordering::Release);
    }
}

#[derive(Default)]
struct KeyStats {
    generation: AtomicU64,
    first: AtomicU64,
    last: AtomicU64,
    count: AtomicU64,
    active: AtomicBool,
    conflict: AtomicBool,
}

impl KeyStats {
    fn begin(&self) -> u64 {
        if self.active.swap(true, Ordering::AcqRel) {
            self.conflict.store(true, Ordering::Release);
        }
        self.first.store(0, Ordering::Relaxed);
        self.last.store(0, Ordering::Relaxed);
        self.count.store(0, Ordering::Relaxed);
        self.generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    }

    fn record(&self, generation: u64, begin: u64, end: u64) {
        if self.generation.load(Ordering::Acquire) != generation {
            return;
        }
        let _ = self
            .first
            .compare_exchange(0, begin, Ordering::Release, Ordering::Relaxed);
        self.last.store(end, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Release);
    }

    fn end(&self, generation: u64) {
        if self.generation.load(Ordering::Acquire) == generation {
            self.active.store(false, Ordering::Release);
        }
    }
}

impl Measurement {
    fn clock(&self) -> u64 {
        let now = monotonic_ns();
        if now == 0 {
            self.failed.store(true, Ordering::Release);
        }
        now
    }

    fn event(&self, event: &'static str) {
        self.record_event(Event {
            event,
            monotonic_ns: self.clock(),
            pid: std::process::id(),
            burst_id: None,
            post_ns: None,
            post_end_ns: None,
        });
    }

    fn record_event(&self, event: Event) {
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if events.len() >= MAX_EVENTS {
            self.failed.store(true, Ordering::Release);
            return;
        }
        events.push(event);
        let mut bytes = Vec::new();
        for event in events.iter() {
            match aterm_json::to_vec(event) {
                Ok(mut line) => {
                    bytes.append(&mut line);
                    bytes.push(b'\n');
                }
                Err(_) => {
                    self.failed.store(true, Ordering::Release);
                    return;
                }
            }
        }
        if self
            .directory
            .view()
            .and_then(|view| view.write(&self.config.events_file, &bytes))
            .is_err()
        {
            self.failed.store(true, Ordering::Release);
        }
    }

    fn wait(&self, name: &str, expected: &[u8]) -> Result<(), String> {
        let until = Instant::now() + HANG;
        loop {
            match read_gate(&self.directory, name, expected)? {
                true => return Ok(()),
                false if Instant::now() < until => std::thread::sleep(POLL),
                false => return Err("measurement gate timed out".into()),
            }
        }
    }
}

fn read_gate(directory: &PrivateDirectory, name: &str, expected: &[u8]) -> Result<bool, String> {
    match directory
        .view()
        .map_err(|error| error.to_string())?
        .read(name, 64)
    {
        Ok(bytes) if bytes == expected => Ok(true),
        Ok(_) => Err("measurement gate has unexpected contents".into()),
        Err(PrivateFileError::Missing) => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

fn load() -> Result<Option<Arc<Measurement>>, String> {
    let Some(path) = aterm_types::dev_seam!("ATERM_DEBUG_RESTORE_MEASUREMENT") else {
        return Ok(None);
    };
    if !cfg!(debug_assertions)
        || !crate::lab_window_custody::present()
        || !aterm_types::dirs::runs_under_private_state_root()
    {
        return Err("restore measurement requires a genuine debug private-lab window".into());
    }
    let path = Path::new(&path);
    if !path.is_absolute() {
        return Err("measurement configuration is not absolute".into());
    }
    let home = aterm_types::dirs::home_dir()
        .ok_or("lab HOME is unavailable")?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let parent = path
        .parent()
        .ok_or("measurement configuration has no parent")?;
    let resolved = parent.canonicalize().map_err(|error| error.to_string())?;
    if resolved == home || !resolved.starts_with(&home) {
        return Err("measurement configuration is outside lab HOME".into());
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("measurement configuration filename is invalid")?;
    let directory = PrivateDirectory::open(parent).map_err(|error| error.to_string())?;
    if !directory
        .is_at(&resolved)
        .map_err(|error| error.to_string())?
    {
        return Err("measurement directory changed".into());
    }
    let bytes = directory
        .view()
        .map_err(|error| error.to_string())?
        .read(name, 16 * 1024)
        .map_err(|error| error.to_string())?;
    let config: Config = aterm_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    config.validate()?;
    for marker in [&config.start_file, &config.events_file]
        .into_iter()
        .chain(config.release_file.iter())
        .chain(config.journal_release_file.iter())
    {
        match directory
            .view()
            .map_err(|error| error.to_string())?
            .read(marker, 64)
        {
            Err(PrivateFileError::Missing) => {}
            _ => return Err("measurement marker already exists or cannot be admitted".into()),
        }
    }
    Ok(Some(Arc::new(Measurement::new(config, directory))))
}

impl Measurement {
    fn new(config: Config, directory: PrivateDirectory) -> Self {
        Self {
            config,
            directory,
            gate: AtomicU8::new(PENDING),
            failed: AtomicBool::new(false),
            wait_taken: AtomicBool::new(false),
            journal_wait_taken: AtomicBool::new(false),
            fixture_taken: AtomicBool::new(false),
            fixture_candidate: Mutex::new(None),
            fixture_retained: AtomicBool::new(false),
            fixture_custody: AtomicU64::new(0),
            fixture_operations: AtomicU64::new(0),
            events: Mutex::new(Vec::new()),
            callbacks: CallbackStats::default(),
            keys: KeyStats::default(),
        }
    }
}

/// Gate before the first restore callback without parking the event loop.
pub(crate) fn start_allowed(proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>) -> bool {
    let loaded = LOADED.get_or_init(|| match load() {
        Ok(None) => Loaded::Absent,
        Err(error) => {
            crate::logging::stderr_line!("restore measurement refused: {error}");
            Loaded::Refused
        }
        Ok(Some(measurement)) => {
            let worker = Arc::clone(&measurement);
            let proxy = proxy.clone();
            if std::thread::Builder::new()
                .name("aterm-restore-measurement-start".into())
                .spawn(move || {
                    let released = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        worker.event("start_ready");
                        worker.wait(&worker.config.start_file, b"start\n").is_ok()
                    }))
                    .unwrap_or(false);
                    if !released {
                        worker.failed.store(true, Ordering::Release);
                    }
                    worker
                        .gate
                        .store(if released { RELEASED } else { FAILED }, Ordering::Release);
                    worker.event(if released {
                        "start_released"
                    } else {
                        "start_failed"
                    });
                    let _ = proxy.send_event(crate::Wake::RestoreMeasurementReady);
                })
                .is_err()
            {
                measurement.failed.store(true, Ordering::Release);
                measurement.gate.store(FAILED, Ordering::Release);
            }
            Loaded::Ready(measurement)
        }
    });
    match loaded {
        Loaded::Ready(measurement) => measurement.gate.load(Ordering::Acquire) != PENDING,
        _ => true,
    }
}

/// Correctness-only hold at the actual shared file-admission boundary.
pub(crate) fn wait_for_uri(uri: &str) {
    wait_for_admission(uri, false);
}

/// Drive private fixture setup independently of whether a cold restore is queued.
/// The existing worker owns the wait and wakes the UI when it has finished.
pub(crate) fn tick_closed_reopen_fixture(app: &mut crate::App) {
    let Some(proxy) = app.proxy.as_ref() else {
        return;
    };
    let _ = start_allowed(proxy);
    if let Some(loaded) = LOADED.get() {
        seed_released_fixture(app, loaded);
    }
}

fn seed_released_fixture(app: &mut crate::App, loaded: &Loaded) {
    let Loaded::Ready(measurement) = loaded else {
        return;
    };
    if !measurement.config.seed_closed_reopen
        || measurement.fixture_taken.load(Ordering::Acquire)
        || measurement.gate.load(Ordering::Acquire) != RELEASED
        || measurement.failed.load(Ordering::Acquire)
        || !app.closed_reopen_fixture_anchor_ready()
        || measurement.fixture_taken.swap(true, Ordering::AcqRel)
    {
        return;
    }
    let seeded = measurement.config.wait_uri.clone().is_some_and(|uri| {
        app.seed_closed_reopen_fixture(uri, measurement.config.seed_closed_view)
    });
    if !seeded {
        measurement.failed.store(true, Ordering::Release);
    }
    measurement.event(if seeded {
        "reopen_fixture_seeded"
    } else {
        "reopen_fixture_refused"
    });
}

/// Setup-only history with a real live terminal anchor, never a document or routing override.
impl crate::App {
    fn closed_reopen_fixture_anchor_ready(&self) -> bool {
        self.frontmost_window
            .and_then(|wid| self.windows.get(&wid))
            .and_then(|window| window.tab_set.active())
            .is_some_and(|tab| {
                tab.root.len() == 1
                    && self
                        .view_store
                        .get(tab.focus)
                        .is_some_and(|view| matches!(view, crate::tab_model::View::Terminal(_)))
            })
    }

    pub(crate) fn seed_closed_reopen_fixture(&mut self, uri: String, view: bool) -> bool {
        use crate::closed_recovery::{ClosedTab, ClosedView, ClosedViewPlacement};
        use crate::restore::{
            NativeLeafRestore, RestoreBranch, RestoredSplitTree, RestoredTab, RestoredView,
            SplitKind,
        };
        let Some(wid) = self
            .frontmost_window
            .filter(|wid| self.windows.contains_key(wid))
        else {
            return false;
        };
        if !self.closed_reopen_fixture_anchor_ready()
            || self.document_store.id_for_uri(&uri).is_some()
        {
            return false;
        }
        let now = self.lat_epoch.elapsed().as_millis() as u64;
        let descriptor = RestoredView::Native(NativeLeafRestore::document("editor", uri));
        if view {
            let Some(tab) = self.windows[&wid].tab_set.active() else {
                return false;
            };
            if tab.root.len() != 1
                || !self
                    .view_store
                    .get(tab.focus)
                    .is_some_and(|view| matches!(view, crate::tab_model::View::Terminal(_)))
            {
                return false;
            }
            self.closed_recovery.views.push(
                ClosedView {
                    original_window: wid,
                    original_tab: tab.id,
                    view: descriptor,
                    placement: ClosedViewPlacement::new(
                        Vec::new(),
                        RestoreBranch::Second,
                        SplitKind::Horizontal,
                        0.37,
                    )
                    .expect("fixed valid fixture placement"),
                },
                now,
            );
        } else {
            self.closed_recovery.tabs.push(
                ClosedTab {
                    original_window: wid,
                    original_index: self.windows[&wid].tab_set.len(),
                    tab: RestoredTab {
                        root: RestoredSplitTree::leaf(descriptor),
                        focused_path: Vec::new(),
                        zoomed: false,
                        parked: false,
                        ephemeral: false,
                        fleet: false,
                    },
                },
                now,
            );
        }
        if let Some(Loaded::Ready(measurement)) = LOADED.get() {
            let candidate = if view {
                self.closed_recovery
                    .views
                    .candidate_snapshot(now)
                    .map(|candidate| candidate.token)
            } else {
                self.closed_recovery
                    .tabs
                    .candidate_snapshot(now)
                    .map(|candidate| candidate.token)
            };
            *measurement
                .fixture_candidate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                candidate.map(|token| (view, token));
        }
        observe_closed_reopen_fixture(self);
        true
    }
}

/// Read-only fixture evidence, sampled on UI after the actual operation progresses.
pub(crate) fn observe_closed_reopen_fixture(app: &crate::App) {
    let Some(Loaded::Ready(measurement)) = LOADED.get() else {
        return;
    };
    if !measurement.config.seed_closed_reopen {
        return;
    }
    let candidate = *measurement
        .fixture_candidate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = app.lat_epoch.elapsed().as_millis() as u64;
    let retained = candidate.is_some_and(|(view, token)| {
        if view {
            app.closed_recovery.views.matches_candidate(token, now)
        } else {
            app.closed_recovery.tabs.matches_candidate(token, now)
        }
    });
    measurement
        .fixture_retained
        .store(retained, Ordering::Release);
    measurement.fixture_custody.store(
        app.unpublished_attachment_count_for_test() as u64,
        Ordering::Release,
    );
    measurement
        .fixture_operations
        .store(app.closed_reopens.len() as u64, Ordering::Release);
}

/// Observe the ordinary predicate without changing it.
pub(crate) fn closed_reopen_canonical(uri: &str, present: bool) {
    if let Some(Loaded::Ready(measurement)) = LOADED.get()
        && measurement.config.closed_reopen
        && measurement.config.wait_uri.as_deref() == Some(uri)
    {
        measurement.event(if present {
            "reopen_canonical_present"
        } else {
            "reopen_canonical_absent"
        });
    }
}

pub(crate) fn closed_reopen_reused(uri: &str) {
    if let Some(Loaded::Ready(measurement)) = LOADED.get()
        && measurement.config.closed_reopen
        && measurement.config.wait_uri.as_deref() == Some(uri)
    {
        measurement.event("reopen_canonical_reused");
    }
}

/// Hold the actual detached reader, after an ordinary open/close created its history.
pub(crate) fn wait_for_detached_read(uri: &str) {
    wait_for_admission(uri, true);
}

fn wait_for_admission(uri: &str, detached: bool) {
    let Some(Loaded::Ready(measurement)) = LOADED.get() else {
        return;
    };
    if measurement.config.closed_reopen != detached
        || measurement.gate.load(Ordering::Acquire) != RELEASED
        || measurement.config.wait_uri.as_deref() != Some(uri)
        || measurement.wait_taken.swap(true, Ordering::AcqRel)
    {
        return;
    }
    let Some(release) = measurement.config.release_file.as_deref() else {
        return;
    };
    measurement.event("wait_entered");
    let released = measurement.wait(release, b"release\n").is_ok();
    if !released {
        measurement.failed.store(true, Ordering::Release);
    }
    measurement.event(if released {
        "wait_released"
    } else {
        "wait_failed"
    });
}

/// Hold the real preparation worker after its grant work, before journal admission.
/// The detached read must already have entered; ordinary fixture opens cannot take it.
pub(crate) fn wait_for_reopen_journal(uri: &str) {
    let Some(Loaded::Ready(measurement)) = LOADED.get() else {
        return;
    };
    if !measurement.config.closed_reopen
        || measurement.config.wait_uri.as_deref() != Some(uri)
        || !measurement.wait_taken.load(Ordering::Acquire)
    {
        return;
    }
    let Some(release) = measurement.config.journal_release_file.as_deref() else {
        return;
    };
    if measurement.journal_wait_taken.swap(true, Ordering::AcqRel) {
        return;
    }
    measurement.event("journal_wait_entered");
    let released = measurement.wait(release, b"release\n").is_ok();
    if !released {
        measurement.failed.store(true, Ordering::Release);
    }
    measurement.event(if released {
        "journal_wait_released"
    } else {
        "journal_wait_failed"
    });
}

pub(crate) struct Callback {
    measurement: Option<Arc<Measurement>>,
    begin: u64,
    finished: bool,
}

impl Callback {
    pub(crate) fn begin() -> Self {
        let measurement = match LOADED.get() {
            Some(Loaded::Ready(measurement)) => Some(Arc::clone(measurement)),
            _ => None,
        };
        let begin = measurement.as_ref().map_or(0, |measurement| {
            measurement.callbacks.active.fetch_add(1, Ordering::Relaxed);
            measurement.clock()
        });
        Self {
            begin,
            measurement,
            finished: false,
        }
    }
    pub(crate) fn finished(&mut self) {
        self.finished = true;
    }
}

impl Drop for Callback {
    fn drop(&mut self) {
        if let Some(measurement) = &self.measurement {
            measurement
                .callbacks
                .record(self.begin, measurement.clock(), self.finished);
            measurement.callbacks.active.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

pub(crate) struct KeyBurst {
    measurement: Option<Arc<Measurement>>,
    generation: u64,
    posts: Vec<(u64, u64)>,
    limit: usize,
}

impl KeyBurst {
    pub(crate) fn begin(count: u32) -> Self {
        let measurement = match LOADED.get() {
            Some(Loaded::Ready(measurement)) => Some(Arc::clone(measurement)),
            _ => None,
        };
        let generation = measurement
            .as_ref()
            .map_or(0, |measurement| measurement.keys.begin());
        let limit = usize::try_from(count)
            .unwrap_or(MAX_POSTS + 1)
            .min(MAX_POSTS);
        let posts = if measurement.is_some() {
            Vec::with_capacity(limit)
        } else {
            Vec::new()
        };
        Self {
            measurement,
            generation,
            posts,
            limit,
        }
    }
    pub(crate) fn start_post(&self) -> u64 {
        self.measurement
            .as_ref()
            .map_or(0, |measurement| measurement.clock())
    }
    pub(crate) fn posted(&mut self, begin: u64) {
        let Some(measurement) = &self.measurement else {
            return;
        };
        let end = measurement.clock();
        measurement.keys.record(self.generation, begin, end);
        if self.posts.len() < self.limit {
            self.posts.push((begin, end));
        } else {
            measurement.failed.store(true, Ordering::Release);
        }
    }
}

impl Drop for KeyBurst {
    fn drop(&mut self) {
        if let Some(measurement) = &self.measurement {
            measurement.keys.end(self.generation);
            let posts = std::mem::take(&mut self.posts);
            measurement.record_event(Event {
                event: "key_posts",
                monotonic_ns: measurement.clock(),
                pid: std::process::id(),
                burst_id: Some(self.generation),
                post_ns: Some(posts.iter().map(|(begin, _)| *begin).collect()),
                post_end_ns: Some(posts.iter().map(|(_, end)| *end).collect()),
            });
        }
    }
}

pub(crate) fn reset() {
    if let Some(Loaded::Ready(measurement)) = LOADED.get() {
        if measurement.callbacks.active.load(Ordering::Acquire) > 0
            || measurement.keys.active.load(Ordering::Acquire)
        {
            measurement.failed.store(true, Ordering::Release);
        }
        measurement.callbacks.reset();
        measurement.keys.first.store(0, Ordering::Relaxed);
        measurement.keys.last.store(0, Ordering::Relaxed);
        measurement.keys.count.store(0, Ordering::Release);
        measurement.keys.conflict.store(false, Ordering::Release);
    }
}

pub(crate) fn emit(sink: &mut dyn crate::metrics::wire::FieldSink) {
    let Some(loaded) = LOADED.get() else { return };
    let Loaded::Ready(measurement) = loaded else {
        if matches!(loaded, Loaded::Refused) {
            sink.count("restore_measurement_enabled", 0);
            sink.count("restore_measurement_refused", 1);
        }
        return;
    };
    let c = &measurement.callbacks;
    let k = &measurement.keys;
    let finished = c.finished.load(Ordering::Acquire);
    let key_active = k.active.load(Ordering::Acquire);
    let key_count = k.count.load(Ordering::Acquire);
    for (name, value) in [
        ("enabled", 1),
        (
            "fixture_candidate_retained",
            u64::from(measurement.fixture_retained.load(Ordering::Acquire)),
        ),
        (
            "fixture_unpublished_attachments",
            measurement.fixture_custody.load(Ordering::Acquire),
        ),
        (
            "fixture_closed_operations",
            measurement.fixture_operations.load(Ordering::Acquire),
        ),
        (
            "refused",
            u64::from(measurement.failed.load(Ordering::Acquire)),
        ),
        (
            "gate_released",
            u64::from(measurement.gate.load(Ordering::Acquire) == RELEASED),
        ),
        ("callback_count", c.count.load(Ordering::Acquire)),
        ("callback_max_ns", c.max.load(Ordering::Acquire)),
        ("callback_max_begin_ns", c.max_begin.load(Ordering::Relaxed)),
        ("callback_max_end_ns", c.max_end.load(Ordering::Relaxed)),
        ("callback_first_begin_ns", c.first.load(Ordering::Relaxed)),
        ("callback_last_end_ns", c.last.load(Ordering::Relaxed)),
        ("callback_total_ns", c.total.load(Ordering::Relaxed)),
        ("finished", u64::from(finished)),
        ("key_burst_id", k.generation.load(Ordering::Acquire)),
        ("key_first_post_ns", k.first.load(Ordering::Acquire)),
        ("key_last_post_ns", k.last.load(Ordering::Relaxed)),
        ("key_posted", key_count),
        ("key_active", u64::from(key_active)),
        (
            "key_conflict",
            u64::from(k.conflict.load(Ordering::Acquire)),
        ),
    ] {
        sink.count(&format!("restore_measurement_{name}"), value);
    }
}

pub(crate) fn monotonic_ns() -> u64 {
    let mut stamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: CLOCK_MONOTONIC is available and `stamp` is a writable timespec.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut stamp) } != 0 {
        return 0;
    }
    u64::try_from(stamp.tv_sec)
        .unwrap_or(0)
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::try_from(stamp.tv_nsec).unwrap_or(0))
}

#[cfg(test)]
#[path = "restore_measurement_tests.rs"]
mod tests;
