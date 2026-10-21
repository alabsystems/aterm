// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE VITALS HOST: the one probe thread that reads the machine for the stats
//! Deck (`crate::stats_deck`), and the loop's side of it.
//!
//! The thread owns the platform readers (`aterm_sysprobe`) and the model
//! (`aterm_vitals::Vitals`). The loop sends it a [`Request`] when a reading is
//! due ([`App::service_vitals`]): this process's pid, each tab's shell and the
//! front tab's shell (whose working directory names the volume to report). The
//! thread reads, folds the reading into the trends, and leaves the new
//! [`Snapshot`] in a shared slot before posting `Wake::Vitals`. The loop takes
//! the slot on that wake AND on every due tick, because a wake can be dropped
//! (an update handoff's gate) and the Deck must not stall behind it.
//!
//! Cadence (design §6.2): two readings a second while a lens is open, one a
//! second while a focused window shows the bars (the Deck or the sill), one
//! every five seconds while aterm is in the background. Readings are cheap
//! (design §15.1) but not free, so a hidden Deck schedules none of these.
//!
//! READINGS WITH NO WINDOW (the hung-call design's D1, leftover §9): while no
//! window shows the bars, the jobs engine still has to read, or the jobs
//! board the harness workers' stall canary reads goes stale and a hung agent
//! call is never named (no one had the bars open for 26 h in I4). So the
//! harness's disk watch, at each of its minute looks, tells the window it
//! looked (`HostNote::DiskLooked`), and the window takes ONE reading on it
//! ([`App::vitals_disk_looked`]); while the view that reading gave holds a
//! suspect agent call, the next is due at the engine's own cadence
//! ([`crate::vitals_jobs::windowless_after`]: its `Demand`), carried by the
//! loop's deadline fold like any other reading. No new timer and no new
//! thread: the reading is this probe thread's, the beat is the disk
//! watch's. A reading no window shows draws nothing: it is posted to the
//! jobs board, and no Deck is asked for a frame and no strip's meters move.
//! Its cost is the reading's, about 1.5 ms of one core (leftover §9).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_vitals::model::{
    ClusterSpec, CommitSample, GpuSample, MemSample, Pressure, ProcSample, Sample, Snapshot,
    TabRoot, Ticks, VolumeSample,
};

/// A reading a second while a focused window shows the Deck.
pub(crate) const FOCUSED_PERIOD: Duration = Duration::from_secs(1);
/// One every five seconds while aterm is in the background.
pub(crate) const BACKGROUND_PERIOD: Duration = Duration::from_secs(5);

/// What the loop asks the thread to read.
#[derive(Clone, Debug, Default)]
pub(crate) struct Request {
    /// This process's pid (aterm's tree is rooted here).
    pub(crate) self_pid: u32,
    /// Every tab's shell.
    pub(crate) tabs: Vec<TabRoot>,
    /// The front tab's shell, whose working directory names the volume.
    pub(crate) cwd_pid: Option<u32>,
}

/// The loop's side of the probe thread.
pub(crate) struct VitalsHost {
    /// The request channel; `None` until the thread starts (lazily, on the
    /// first due reading) or when it could not start. A test hands its own.
    tx: Option<SyncSender<Request>>,
    /// Set once a start was tried, so a failed start is not retried every tick.
    tried: bool,
    /// Where the thread leaves its newest snapshot.
    latest: Arc<Mutex<Option<Arc<Snapshot>>>>,
    /// The snapshot the Deck draws.
    pub(crate) snapshot: Option<Arc<Snapshot>>,
    /// When `snapshot` was adopted: where a reading no window shows counts
    /// its next from ([`Self::windowless_due`]).
    adopted_at: Option<Instant>,
    /// When the next reading is due: the bars' cadence while a window shows
    /// them; with none, the engine's own while the held view has a live agent
    /// call ([`Self::windowless_due`]); `None` when nothing is due.
    pub(crate) next_due: Option<Instant>,
    /// Whether the last judgement found a window showing the bars: the
    /// hand-over between the two cadences happens where it turns.
    windowed: bool,
    /// The disk watch looked while no window showed the bars, and its one
    /// reading is not asked yet ([`crate::App::vitals_disk_looked`]).
    disk_pull: bool,
    /// Whether the chips' load meters were last handed over with the stats
    /// bars shown ([`crate::App::push_chip_loads`]): turning the bars off
    /// clears the native strips' meters on the next wake.
    chips_shown: bool,
    /// Where each adopted jobs view is posted
    /// ([`crate::App::post_jobs_reading`]): the process's
    /// [`crate::vitals_jobs::BOARD`]; a test's own in a test.
    board: &'static crate::vitals_jobs::JobsBoard,
}

impl Default for VitalsHost {
    fn default() -> Self {
        Self {
            tx: None,
            tried: false,
            latest: Arc::default(),
            snapshot: None,
            adopted_at: None,
            next_due: None,
            windowed: false,
            disk_pull: false,
            chips_shown: false,
            board: &crate::vitals_jobs::BOARD,
        }
    }
}

impl VitalsHost {
    /// Take the thread's newest snapshot if it is newer than the one held,
    /// at `now`. True when the Deck has something new to draw.
    pub(crate) fn adopt(&mut self, now: Instant) -> bool {
        let fresh = self
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        match fresh {
            Some(snap) if self.snapshot.as_ref().is_none_or(|s| s.seq != snap.seq) => {
                self.snapshot = Some(snap);
                self.adopted_at = Some(now);
                true
            }
            _ => false,
        }
    }

    /// When a reading no window shows is due again: the engine's own cadence
    /// from when the held snapshot was adopted, while its jobs view holds a
    /// suspect agent call ([`crate::vitals_jobs::windowless_after`]); `None`
    /// otherwise, and the disk watch's next look asks.
    fn windowless_due(&self) -> Option<Instant> {
        let view = self.snapshot.as_ref()?.jobs.as_deref()?;
        Some(self.adopted_at? + crate::vitals_jobs::windowless_after(view)?)
    }

    /// Start the reader on first use and keep at most one pending request.
    /// A full queue keeps the older request until the reader takes it. With
    /// no event loop (`proxy` `None`: a test App) no thread starts, and only
    /// a reader a test handed over is asked.
    fn request(
        &mut self,
        req: Request,
        proxy: Option<&aterm_window::event_loop::EventLoopProxy<crate::Wake>>,
    ) {
        if let Some(proxy) = proxy.filter(|_| self.tx.is_none() && !self.tried) {
            self.tried = true;
            let proxy = proxy.clone();
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let latest = Arc::clone(&self.latest);
            match crate::qos::spawn_lane_thread(
                "aterm-vitals",
                crate::qos::Role::Background,
                move || run(rx, &latest, &proxy),
            ) {
                Ok(_) => self.tx = Some(tx),
                Err(error) => {
                    aterm_log::warn!("system vitals: the probe thread did not start: {error}");
                }
            }
        }
        self.offer(req);
    }

    /// Offer one reading without waiting for the platform probe.
    fn offer(&mut self, req: Request) {
        if let Some(tx) = self.tx.as_ref() {
            match tx.try_send(req) {
                Ok(()) | Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => {
                    // The thread is gone (it panicked): stop asking.
                    self.tx = None;
                }
            }
        }
    }
}

/// The thread: one [`aterm_sysprobe::Probe`], one [`aterm_vitals::Vitals`],
/// a reading per request. At most one request waits behind a read; while
/// that slot is full the producer keeps the older queued request.
fn run(
    rx: Receiver<Request>,
    latest: &Mutex<Option<Arc<Snapshot>>>,
    proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>,
) {
    let mut reader = Reader::new();
    while let Ok(mut req) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            req = newer;
        }
        let snap = Arc::new(reader.read(&req));
        *latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(snap);
        if proxy.send_event(crate::Wake::Vitals).is_err() {
            return;
        }
    }
}

/// The readers and the model: one per thread.
pub(crate) struct Reader {
    probe: aterm_sysprobe::Probe,
    clusters: Vec<ClusterSpec>,
    vitals: aterm_vitals::Vitals,
    /// The stuck engine's pass over each reading's sweep.
    jobs: crate::vitals_jobs::JobsPass,
    epoch: Instant,
    home: PathBuf,
}

impl Reader {
    pub(crate) fn new() -> Self {
        let clusters = aterm_sysprobe::cpu_layout()
            .clusters
            .into_iter()
            .map(|c| ClusterSpec {
                name: c.name,
                logical: c.logical,
            })
            .collect();
        Self {
            probe: aterm_sysprobe::Probe::new(),
            clusters,
            vitals: aterm_vitals::Vitals::new(),
            jobs: crate::vitals_jobs::JobsPass::new(),
            epoch: Instant::now(),
            home: aterm_types::dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
        }
    }

    /// One reading folded into the trends.
    pub(crate) fn read(&mut self, req: &Request) -> Snapshot {
        let volume_path = req
            .cwd_pid
            .and_then(aterm_sysprobe::process_cwd)
            .unwrap_or_else(|| self.home.clone());
        let sweep = aterm_sysprobe::processes();
        let sample = Sample {
            at_ns: u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX),
            cores: self.probe.core_ticks().map(|cores| {
                cores
                    .iter()
                    .map(|t| Ticks {
                        busy: t.busy(),
                        total: t.total(),
                    })
                    .collect()
            }),
            clusters: self.clusters.clone(),
            mem: aterm_sysprobe::memory_detail().map(mem_sample),
            gpu: aterm_sysprobe::gpu().map(gpu_sample),
            volume: tab_volume(&volume_path).map(|v| VolumeSample {
                total: v.total,
                free: v.free,
                available: v.available,
                mount: v.mount.display().to_string(),
            }),
            procs: sweep.as_ref().map(|sweep| {
                sweep
                    .procs
                    .iter()
                    .map(|p| ProcSample {
                        pid: p.pid,
                        ppid: p.ppid,
                        cpu_ns: p.cpu_ns,
                        footprint: p.footprint,
                        name: p.name.clone(),
                    })
                    .collect()
            }),
            self_pid: req.self_pid,
            tabs: req.tabs.clone(),
        };
        let mut snap = self.vitals.absorb(&sample);
        // The stuck engine reads the same sweep, at its own cadence; a
        // critical memory pressure is the machine thrashing (§9.2), so a wait
        // for pages reads slow, never stuck.
        let thrashing = sample.mem.is_some_and(|m| m.pressure == Pressure::Critical);
        snap.jobs = self.jobs.tick(
            sample.at_ns / 1_000_000,
            sweep.as_ref().map(|s| s.procs.as_slice()),
            &req.tabs,
            req.self_pid,
            thrashing,
        );
        snap
    }
}

/// The machine's memory as the model takes it. A class this OS does not
/// report adds nothing; Linux's shared memory is in use, never cache (design
/// §9.6), so it joins `app`.
fn mem_sample(m: aterm_sysprobe::MemoryDetail) -> MemSample {
    MemSample {
        total: m.total,
        app: m.app.saturating_add(m.shared.unwrap_or(0)),
        wired: m.wired,
        compressed: m.compressed,
        cached: m.file_backed.saturating_add(m.purgeable.unwrap_or(0)),
        free: m.free,
        swap_used: m.swap_used,
        swap_total: m.swap_total,
        pressure: match m.pressure {
            Some(aterm_sysprobe::MemoryLevel::Normal) => Pressure::Normal,
            Some(aterm_sysprobe::MemoryLevel::Warn) => Pressure::Warn,
            Some(aterm_sysprobe::MemoryLevel::Critical) => Pressure::Critical,
            None => Pressure::Unknown,
        },
        commit: m.commit.map(|c| CommitSample {
            charge: c.charge,
            limit: c.limit,
            exact: c.exact,
        }),
    }
}

/// The GPU as the model takes it. A card the OS put to sleep (Linux runtime
/// PM) is doing no work: 0 %, known without waking it.
fn gpu_sample(g: aterm_sysprobe::GpuReading) -> GpuSample {
    let asleep = g.unread == Some(aterm_sysprobe::Why::Asleep);
    GpuSample {
        device_pct: g.device_pct.or(asleep.then_some(0)),
        cores: g.cores,
        name: g.name,
    }
}

/// The front tab's volume. Unix reads it through the mount table
/// ([`local_volume`]); Windows has no table to complete, so the drive is
/// judged from the path's spelling and its drive type, and only a fixed or
/// RAM drive is asked (`aterm_sysprobe::volume`).
fn tab_volume(path: &Path) -> Option<aterm_sysprobe::VolumeReading> {
    #[cfg(windows)]
    {
        aterm_sysprobe::volume(path)
    }
    #[cfg(not(windows))]
    {
        local_volume(
            path,
            &aterm_sysprobe::volumes::table(),
            aterm_sysprobe::volume,
        )
    }
}

/// Read only a mount root that a complete table calls local and not automounted.
/// The tab's directory is used for lexical lookup, never passed to the filesystem
/// reader: it may contain a symlink or lie on an unavailable network mount.
#[cfg(any(not(windows), test))]
fn local_volume(
    path: &Path,
    table: &aterm_sysprobe::volumes::Table,
    mut read: impl FnMut(&Path) -> Option<aterm_sysprobe::VolumeReading>,
) -> Option<aterm_sysprobe::VolumeReading> {
    if !table.complete {
        return None;
    }
    let mount = aterm_sysprobe::volumes::mount_of(path, table)?;
    if !mount.local || mount.automounted || mount.fstype == "autofs" {
        return None;
    }
    read(&mount.on)
}

impl crate::App {
    /// Whether any window shows the stats bars (the Deck or the sill) or the
    /// Live Fleet System Monitor tab (`/tabs` / `/monitor`) now, and whether
    /// aterm is in front. A test App has no loop to start the probe thread
    /// on, so none shows them unless the test handed it a reader.
    fn vitals_audience(&self) -> (bool, bool) {
        if self.proxy.is_none() && self.vitals.tx.is_none() {
            return (false, false);
        }
        let live_window = self
            .windows
            .values()
            .any(|ws| crate::stats_deck::stats_drawn_on(ws) && !ws.occluded);
        let non_occluded = self.windows.values().any(|ws| !ws.occluded);
        let shown = (self.stats_bars_form().shown() && live_window)
            || (self.tabs_page_shown() && non_occluded);
        let focused = self.windows.values().any(|ws| ws.focused);
        (shown, focused)
    }

    /// The deadline the loop folds for the next reading: the bars' cadence
    /// while a window shows the Deck or Fleet Monitor tab; with none, the
    /// jobs engine's own while a suspect agent call stands (module docs);
    /// `None` when nothing is due.
    pub(crate) fn vitals_deadline(&self) -> Option<Instant> {
        self.vitals.next_due
    }

    /// A level judged every wake: adopt a snapshot the thread left, and when a
    /// reading is due, ask for the next one. A hidden Deck / Monitor tab
    /// hands the cadence to the jobs engine's ([`Self::serve_windowless_vitals`]).
    pub(crate) fn service_vitals(&mut self, now: Instant) {
        let (shown, focused) = self.vitals_audience();
        let chips_shown = self.stats_bars_form().shown();
        let adopted = self.adopt_vitals(shown, now);
        // A reading no window shows draws nothing; turning the bars off
        // still clears the strips' meters.
        if (adopted && shown) || chips_shown != self.vitals.chips_shown {
            self.vitals.chips_shown = chips_shown;
            self.show_vitals_reading();
        }
        if !shown {
            self.serve_windowless_vitals(now);
            return;
        }
        if !std::mem::replace(&mut self.vitals.windowed, true) {
            // A window shows the bars again: read at once, as at the first.
            self.vitals.next_due = None;
        }
        self.vitals.disk_pull = false;
        if self.vitals.next_due.is_some_and(|due| due > now) {
            return;
        }
        let period = if self.any_stats_lens_open() {
            crate::stats_lens::LENS_PERIOD
        } else if focused {
            FOCUSED_PERIOD
        } else {
            BACKGROUND_PERIOD
        };
        self.vitals.next_due = Some(now + period);
        let req = self.vitals_request();
        self.vitals.request(req, self.proxy.as_ref());
        if self.tabs_page_shown() && self.harness_screen.fleet_sweep_due(now) {
            self.start_harness_stats_refresh_at(now);
        }
    }

    /// NO WINDOW SHOWS THE BARS (module docs, the hung-call design's D1): ask
    /// for one reading when the disk watch looked or the held view's engine
    /// demand is due, and nothing else. The next is named when that reading
    /// lands ([`Self::adopt_vitals`]), from the view it gave; until then
    /// nothing more is asked. The bars just hidden, their cadence stops and
    /// the held view's demand goes on.
    fn serve_windowless_vitals(&mut self, now: Instant) {
        if std::mem::replace(&mut self.vitals.windowed, false) {
            self.vitals.next_due = self.vitals.windowless_due();
        }
        let pulled = std::mem::take(&mut self.vitals.disk_pull);
        if !pulled && self.vitals.next_due.is_none_or(|due| due > now) {
            return;
        }
        self.vitals.next_due = None;
        let req = self.vitals_request();
        self.vitals.request(req, self.proxy.as_ref());
    }

    /// THE DISK WATCH LOOKED (`HostNote::DiskLooked`, once a
    /// `harness_host::DISK_TICK`): with no window showing the bars, take one
    /// reading now, so the jobs board speaks of now (module docs). With one,
    /// the bars' own cadence reads, and the look changes nothing.
    pub(crate) fn vitals_disk_looked(&mut self, now: Instant) {
        if self.vitals_audience().0 {
            return;
        }
        self.vitals.disk_pull = true;
        self.service_vitals(now);
    }

    /// `Wake::Vitals`: the thread left a snapshot.
    pub(crate) fn on_vitals_wake(&mut self) {
        let (shown, _) = self.vitals_audience();
        if self.adopt_vitals(shown, Instant::now()) && shown {
            self.show_vitals_reading();
        }
    }

    /// Adopt a snapshot the thread left, at `now`, and post its jobs view to
    /// the board; with no window showing the bars (`shown` false), name the
    /// next reading from that view ([`VitalsHost::windowless_due`]). True
    /// when one was adopted.
    fn adopt_vitals(&mut self, shown: bool, now: Instant) -> bool {
        if !self.vitals.adopt(now) {
            return false;
        }
        self.post_jobs_reading();
        if !shown {
            self.vitals.next_due = self.vitals.windowless_due();
        }
        true
    }

    /// A new reading for whatever shows one: every Deck asked for a frame,
    /// the strips' meters, the Fleet Monitor tab.
    fn show_vitals_reading(&mut self) {
        self.redraw_stats_decks();
        self.push_chip_loads();
        if self.tabs_page_shown() {
            self.publish_native_harness_state();
        }
    }

    /// Post the adopted snapshot's jobs view to the harness workers' board
    /// (`VitalsHost`'s: [`crate::vitals_jobs::BOARD`] live), each row's tab (the pool session's
    /// local id, [`Self::vitals_request`]'s `TabRoot::key`) mapped to its
    /// session id under one read of the store. A snapshot with no view posts
    /// nothing: the board's last view ages out on its own
    /// ([`crate::vitals_jobs::JOBS_FRESH`]).
    fn post_jobs_reading(&self) {
        let Some(view) = self
            .vitals
            .snapshot
            .as_ref()
            .and_then(|snap| snap.jobs.clone())
        else {
            return;
        };
        let sids = {
            let store = self.store.read().unwrap_or_else(|p| p.into_inner());
            view.rows
                .iter()
                .filter_map(|r| {
                    store
                        .by_local(r.tab)
                        .map(|h| (r.tab, h.sid.as_str().to_string()))
                })
                .collect()
        };
        self.vitals.board.post(view, sids, Instant::now());
    }

    /// One tab's load step for its chip's meter (design §12.4): its focused
    /// session's cores-worth on the machine's capacity
    /// ([`crate::tab_bar::chip_load_step`]). 0 (no meter) in a one-tab window
    /// (its chip is the window title), while the stats bars are off (nothing
    /// is read, so a reading would be stale), and before the first reading.
    pub(crate) fn chip_load(&self, tab_count: usize, focus: crate::tab_model::ViewId) -> u8 {
        if tab_count < 2 || !self.stats_bars_form().shown() {
            return 0;
        }
        let Some(snap) = self.vitals.snapshot.as_deref() else {
            return 0;
        };
        let capacity = snap.total_cores().unwrap_or(0) as f32;
        self.view_store
            .get(focus)
            .copied()
            .and_then(crate::tab_model::View::terminal_session)
            .and_then(|session| snap.tab(session))
            .map_or(0, |tab| {
                crate::tab_bar::chip_load_step(tab.scope.cores.level, capacity)
            })
    }

    /// Hand each native strip its chips' load steps and the meters' inks
    /// after a reading: a repaint of the chips whose step moved, never a
    /// strip rebuild. The in-grid strip reads the same steps through
    /// `stamp_presence_chips` on the frame the reading asks for.
    pub(crate) fn push_chip_loads(&self) {
        if self._toolbars.is_empty() {
            return;
        }
        let band = crate::chrome_band::band_colors(
            self.chrome_palette(crate::app_render::ChromeSurface::Header),
        );
        let inks = crate::stats_deck::cached_inks(band.bar_bg, band.value, band.accent);
        for (wid, handle) in &self._toolbars {
            let Some(ws) = self.windows.get(wid) else {
                continue;
            };
            let n = ws.tab_set.len();
            let loads: Vec<u8> = ws
                .tab_set
                .tabs()
                .iter()
                .map(|tab| self.chip_load(n, tab.focus))
                .collect();
            crate::toolbar::set_chip_loads(handle, &loads, [inks.tab, inks.aterm]);
        }
    }

    /// Ask each window showing the Deck for a frame; the Deck's fingerprint
    /// decides whether one presents.
    fn redraw_stats_decks(&self) {
        for ws in self.windows.values() {
            if let Some(window) = ws.os_window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// Every tab's shell and the front tab's, for the thread.
    fn vitals_request(&self) -> Request {
        let mut tabs = Vec::new();
        let mut cwd_pid = None;
        let front = self
            .windows
            .iter()
            .find(|(_, ws)| ws.focused)
            .or_else(|| self.windows.iter().next())
            .map(|(id, _)| *id);
        for (wid, ws) in &self.windows {
            let active = ws.tab_set.active_index();
            for (i, tab) in ws.tab_set.tabs().iter().enumerate() {
                let Some(crate::tab_model::View::Terminal(view)) =
                    self.view_store.get(tab.focus).copied()
                else {
                    continue;
                };
                let Some(s) = self.pool.get(view.session) else {
                    continue;
                };
                let Ok(pid) = u32::try_from(s.pid) else {
                    continue;
                };
                if pid == 0 {
                    continue;
                }
                if Some(*wid) == front && active == Some(i) {
                    cwd_pid = Some(pid);
                }
                tabs.push(TabRoot {
                    key: view.session,
                    shell_pid: pid,
                    label: format!("Tab {}", i + 1),
                    program: String::new(),
                });
            }
        }
        Request {
            self_pid: std::process::id(),
            tabs,
            cwd_pid,
        }
    }

    /// This window's front tab: its session id and label (`Tab 2`).
    pub(crate) fn deck_tab(&self, wid: crate::WindowId) -> (Option<u64>, String) {
        let Some(ws) = self.windows.get(&wid) else {
            return (None, "Tab".into());
        };
        let Some(i) = ws.tab_set.active_index() else {
            return (None, "Tab".into());
        };
        let label = format!("Tab {}", i + 1);
        let key =
            ws.tab_set
                .tab_at(i)
                .and_then(|tab| match self.view_store.get(tab.focus).copied() {
                    Some(crate::tab_model::View::Terminal(view)) => Some(view.session),
                    _ => None,
                });
        (key, label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_agent::supervise::BackgroundEvidence;
    use aterm_vitals::jobs::{Consumer, Demand, JobRow, JobsView};

    use crate::harness_host::{DISK_TICK, HostNote};
    use crate::vitals_jobs::JobsBoard;

    /// A test App with a reader in hand instead of the probe thread (none
    /// starts: a test App has no loop), posting to `board`. The receiver is
    /// the thread's end: each request the App asks lands there.
    fn with_reader(app: &mut crate::App, board: &'static JobsBoard) -> Receiver<Request> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        app.vitals.tx = Some(tx);
        app.vitals.tried = true;
        app.vitals.board = board;
        rx
    }

    /// The readings asked since the last look.
    fn asked(rx: &Receiver<Request>) -> usize {
        rx.try_iter().count()
    }

    /// The probe thread's part: reading `seq` lands with a jobs view of
    /// `rows` whose engine asked `demand`.
    fn land(app: &crate::App, seq: u64, rows: Vec<JobRow>, demand: Demand) {
        let view = JobsView {
            rows,
            demand,
            ..JobsView::default()
        };
        *app.vitals.latest.lock().unwrap() = Some(Arc::new(Snapshot {
            seq,
            jobs: Some(Arc::new(view)),
            ..Snapshot::default()
        }));
    }

    /// Tab 0's (the pool session's local id 0) session id.
    fn sid_of_tab0(app: &crate::App) -> String {
        let store = app.store.read().unwrap();
        store
            .by_local(0)
            .expect("tab 0's session")
            .sid
            .as_str()
            .to_owned()
    }

    /// I4's call: tab 0's agent's background call, suspect (capped: an
    /// unnamed wait) and at no progress for 31 minutes, waiting in pid 27098.
    fn hung_call() -> JobRow {
        JobRow {
            standing: Some(aterm_vitals::jobs::Standing::Capped),
            ..crate::vitals_jobs::tests::call_row(0, Consumer::AgentBg, true, 31, 27098, "python3")
        }
    }

    /// READINGS WITH NO WINDOW (the hung-call design's D1): no window shows
    /// the bars, so nothing is read and nothing is due, until the disk
    /// watch's look, which asks for ONE reading and nothing more while it is
    /// read. Landed, its jobs view is on the board, where the canary's
    /// evidence for tab 0's session names the hung call. While that suspect
    /// agent call stands, the engine's own demand (fast: 2 s from the
    /// reading's adoption) names the next reading, and the loop asks it then,
    /// not before; once a reading shows no agent call, nothing is due until
    /// the next look. NEGATIVE CONTROL: before the look, a wake asks nothing.
    #[test]
    fn with_no_window_the_disk_look_reads_once_and_the_board_names_the_hung_call() {
        static BOARD: JobsBoard = JobsBoard::new();
        let mut app = crate::App::headless_for_test();
        let rx = with_reader(&mut app, &BOARD);
        let sid = sid_of_tab0(&app);
        app.service_vitals(Instant::now());
        assert_eq!(asked(&rx), 0, "no window and no look: nothing is read");
        assert_eq!(app.vitals_deadline(), None);

        app.note_harness_host(HostNote::DiskLooked);
        assert_eq!(asked(&rx), 1, "the look's one reading");
        app.service_vitals(Instant::now());
        assert_eq!(asked(&rx), 0, "nothing more while it is read");

        land(&app, 1, vec![hung_call()], Demand::Fast);
        app.on_vitals_wake();
        let posted = Instant::now();
        let named = |at| match BOARD.evidence(&sid, at) {
            BackgroundEvidence::NoProgress { worst } => Some((worst.pid, worst.quiet_min())),
            BackgroundEvidence::Unknown => None,
        };
        assert_eq!(named(posted), Some((27098, 31)), "the board speaks");

        let due = app
            .vitals_deadline()
            .expect("the engine's demand names the next reading");
        assert_eq!(
            Some(due),
            app.vitals.adopted_at.map(|at| at + Duration::from_secs(2)),
            "the engine's fast cadence, from the adoption"
        );
        app.service_vitals(due - Duration::from_millis(1));
        assert_eq!(asked(&rx), 0, "not before the engine's cadence");
        app.service_vitals(due);
        assert_eq!(asked(&rx), 1, "the engine's demand reads");
        assert_eq!(app.vitals_deadline(), None, "named again when it lands");

        land(&app, 2, Vec::new(), Demand::Idle);
        app.on_vitals_wake();
        assert_eq!(app.vitals_deadline(), None, "no agent call: nothing due");
        app.service_vitals(due + DISK_TICK / 2);
        assert_eq!(asked(&rx), 0, "nothing more within the tick");
    }

    /// WITH A WINDOW SHOWING THE BARS NOTHING CHANGES: the bars' cadence
    /// reads at once and then a period on; the disk watch's look asks
    /// nothing, and a jobs view whose engine is fast moves no deadline. The
    /// bars hidden, the held view's demand goes on (slow, 30 s: a deadline,
    /// no reading yet); shown again, the bars read at once.
    #[cfg(unix)]
    #[test]
    fn with_a_window_showing_the_bars_the_look_and_the_engine_move_nothing() {
        static BOARD: JobsBoard = JobsBoard::new();
        let bars = crate::stats_deck::tests::attach_stats();
        let mut app = crate::App::headless_for_test();
        let rx = with_reader(&mut app, &BOARD);
        let t0 = Instant::now();
        app.service_vitals(t0);
        assert_eq!(asked(&rx), 1, "read at once");
        let first = app.vitals_deadline().expect("the bars' cadence");
        let period = first - t0;
        assert!(
            [FOCUSED_PERIOD, BACKGROUND_PERIOD].contains(&period),
            "{period:?}"
        );

        app.note_harness_host(HostNote::DiskLooked);
        assert_eq!((asked(&rx), app.vitals_deadline()), (0, Some(first)));
        land(&app, 1, vec![hung_call()], Demand::Fast);
        app.on_vitals_wake();
        assert_eq!(
            app.vitals_deadline(),
            Some(first),
            "the demand moves nothing"
        );
        app.service_vitals(first);
        assert_eq!(
            (asked(&rx), app.vitals_deadline()),
            (1, Some(first + period))
        );
        land(&app, 2, vec![hung_call()], Demand::Slow);
        app.on_vitals_wake();

        drop(bars);
        app.service_vitals(first + Duration::from_millis(1));
        assert_eq!(asked(&rx), 0);
        assert_eq!(
            app.vitals_deadline(),
            app.vitals.adopted_at.map(|at| at + Duration::from_secs(30)),
            "the held view's demand (slow)"
        );
        let _bars = crate::stats_deck::tests::attach_stats();
        app.service_vitals(first + Duration::from_millis(2));
        assert_eq!(asked(&rx), 1, "shown again: read at once");
    }

    /// The production admission uses the table without touching the tab's path.
    /// Refused and incomplete readings never reach even an injected reader.
    #[test]
    fn volume_reads_use_only_complete_local_mount_roots() {
        use aterm_sysprobe::volumes::{Mount, Table};
        let mount = |on: &str, local, automounted, fstype: &str| Mount {
            on: PathBuf::from(on),
            from: String::new(),
            fstype: fstype.to_owned(),
            local,
            automounted,
            read_only: false,
            container: None,
        };
        let table = Table {
            mounts: vec![
                mount("/", true, false, "apfs"),
                mount("/System/Volumes/Data", true, false, "apfs"),
                mount("/Volumes/Remote", false, false, "smbfs"),
                mount("/auto", true, true, "apfs"),
                mount("/autofs", true, false, "autofs"),
            ],
            complete: true,
            firmlinks: vec![(
                PathBuf::from("/Users"),
                PathBuf::from("/System/Volumes/Data/Users"),
            )],
        };
        let expected = aterm_sysprobe::VolumeReading {
            total: 100,
            free: 70,
            available: 60,
            mount: PathBuf::from("/System/Volumes/Data"),
            fs: "apfs".to_owned(),
        };
        let mut calls = Vec::new();
        assert_eq!(
            local_volume(Path::new("/Users//a/project/link"), &table, |path| {
                calls.push(path.to_path_buf());
                Some(expected.clone())
            }),
            Some(expected)
        );
        assert_eq!(calls, [PathBuf::from("/System/Volumes/Data")]);
        for path in [
            "/Volumes/Remote/offline",
            "/auto/offline",
            "/autofs/offline",
            "relative",
            "/Users//a/../elsewhere",
        ] {
            assert_eq!(
                local_volume(Path::new(path), &table, |_| panic!("unadmitted {path}")),
                None
            );
        }
        let mut incomplete = table;
        incomplete.complete = false;
        assert_eq!(
            local_volume(Path::new("/Users//a/project"), &incomplete, |_| {
                panic!("an incomplete table cannot rule out a nested remote mount")
            }),
            None
        );
    }

    /// Linux's and Windows' readings reach the model: shared memory counts
    /// as in use, a class the OS does not report adds nothing, and commit
    /// keeps whether its limit is enforced. A NEGATIVE CONTROL: macOS's
    /// purgeable pages are still cache.
    #[test]
    fn readings_of_every_os_map_to_the_models_classes() {
        let linux = aterm_sysprobe::MemoryDetail {
            total: 32 << 30,
            free: 2 << 30,
            app: 9 << 30,
            shared: Some(1 << 30),
            wired: 1 << 30,
            file_backed: 11 << 30,
            commit: Some(aterm_sysprobe::Commit {
                charge: 18 << 30,
                limit: 24 << 30,
                exact: false,
            }),
            pressure: None,
            ..aterm_sysprobe::MemoryDetail::default()
        };
        let m = mem_sample(linux);
        assert_eq!((m.app, m.cached), (10 << 30, 11 << 30));
        assert_eq!(m.compressed, None, "an unreported class is not a zero");
        assert_eq!(m.pressure, Pressure::Unknown, "no PSI is not normal");
        assert_eq!(
            m.commit,
            Some(CommitSample {
                charge: 18 << 30,
                limit: 24 << 30,
                exact: false
            })
        );
        let mac = aterm_sysprobe::MemoryDetail {
            file_backed: 3 << 30,
            purgeable: Some(1 << 30),
            compressed: Some(2 << 30),
            exact: true,
            ..aterm_sysprobe::MemoryDetail::default()
        };
        let m = mem_sample(mac);
        assert_eq!(
            (m.cached, m.compressed, m.commit),
            (4 << 30, Some(2 << 30), None)
        );

        let gpu = |device_pct, unread| aterm_sysprobe::GpuReading {
            device_pct,
            unread,
            name: Some("amdgpu".into()),
            ..aterm_sysprobe::GpuReading::default()
        };
        let asleep = gpu(None, Some(aterm_sysprobe::Why::Asleep));
        assert_eq!(gpu_sample(asleep).device_pct, Some(0), "asleep is idle");
        let privileged = gpu(
            None,
            Some(aterm_sysprobe::Why::NeedsPrivilege("cap_perfmon")),
        );
        assert_eq!(gpu_sample(privileged).device_pct, None, "unread is not 0");
        assert_eq!(gpu_sample(gpu(Some(37), None)).device_pct, Some(37));
    }

    #[test]
    fn two_readings_give_this_process_a_scope_and_the_machine_its_cores() {
        let mut reader = Reader::new();
        let req = Request {
            self_pid: std::process::id(),
            tabs: Vec::new(),
            cwd_pid: None,
        };
        let _ = reader.read(&req);
        // Spend a little CPU so the second reading has a delta to show.
        let mut x = 0u64;
        for i in 0..2_000_000u64 {
            x = x.wrapping_mul(31).wrapping_add(i);
        }
        assert_ne!(x, 1);
        let snap = reader.read(&req);
        if cfg!(any(
            target_os = "macos",
            target_os = "linux",
            all(windows, target_pointer_width = "64")
        )) {
            let cpu = snap.cpu.as_ref().expect("cores are read");
            assert!(cpu.total() > 0);
            assert!(snap.mem.is_some(), "memory is read");
            assert!(snap.volume.is_some(), "the home volume is read");
            assert!(snap.aterm.is_some(), "this process is aterm's root");
        }
    }

    /// The production producer retains one pending request while a platform
    /// read is busy. Later ticks forgo their request instead of accumulating
    /// work or waiting for the reader; room and disconnect behave normally.
    #[test]
    fn a_busy_reader_keeps_one_pending_request_and_forgoes_newer_work() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let mut host = VitalsHost {
            tx: Some(tx),
            tried: true,
            ..VitalsHost::default()
        };
        let request = |pid| Request {
            self_pid: pid,
            ..Request::default()
        };
        host.offer(request(1));
        host.offer(request(2));
        assert_eq!(rx.try_recv().expect("one pending reading").self_pid, 1);
        assert!(matches!(
            rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        host.offer(request(3));
        assert_eq!(
            rx.try_recv()
                .expect("room for the current reading")
                .self_pid,
            3
        );
        drop(rx);
        host.offer(request(4));
        assert!(host.tx.is_none(), "a reader that ended is not asked again");
        host.offer(request(5));
        assert!(host.tx.is_none());
        assert!(host.tried, "no reader is restarted after disconnect");
    }
}
