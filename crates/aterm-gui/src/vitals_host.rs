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
//! Cadence (design §6.2): one reading a second while a focused window shows the
//! Deck, one every five seconds while aterm is in the background, none while
//! no window shows it. Readings are cheap (design §15.1) but not free, so a
//! hidden Deck schedules no new requests; a pending reading may still finish.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_vitals::model::{
    ClusterSpec, GpuSample, MemSample, Pressure, ProcSample, Sample, Snapshot, TabRoot, Ticks,
    VolumeSample,
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
#[derive(Default)]
pub(crate) struct VitalsHost {
    /// The request channel; `None` until the thread starts (lazily, on the
    /// first due reading) or when it could not start.
    tx: Option<SyncSender<Request>>,
    /// Set once a start was tried, so a failed start is not retried every tick.
    tried: bool,
    /// Where the thread leaves its newest snapshot.
    latest: Arc<Mutex<Option<Arc<Snapshot>>>>,
    /// The snapshot the Deck draws.
    pub(crate) snapshot: Option<Arc<Snapshot>>,
    /// When the next reading is due; `None` while no window shows the Deck.
    pub(crate) next_due: Option<Instant>,
}

impl VitalsHost {
    /// Take the thread's newest snapshot if it is newer than the one held.
    /// True when the Deck has something new to draw.
    pub(crate) fn adopt(&mut self) -> bool {
        let fresh = self
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        match fresh {
            Some(snap) if self.snapshot.as_ref().is_none_or(|s| s.seq != snap.seq) => {
                self.snapshot = Some(snap);
                true
            }
            _ => false,
        }
    }

    /// Start the reader on first use and keep at most one pending request.
    /// A full queue keeps the older request until the reader takes it. A test
    /// App without an event loop stops at `service_vitals`.
    fn request(
        &mut self,
        req: Request,
        proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    ) {
        if self.tx.is_none() && !self.tried {
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
            mem: aterm_sysprobe::memory_detail().map(|m| MemSample {
                total: m.total,
                app: m.app,
                wired: m.wired,
                compressed: m.compressed,
                cached: m.file_backed.saturating_add(m.purgeable),
                free: m.free,
                swap_used: m.swap_used,
                swap_total: m.swap_total,
                pressure: match m.pressure {
                    Some(aterm_sysprobe::MemoryLevel::Normal) => Pressure::Normal,
                    Some(aterm_sysprobe::MemoryLevel::Warn) => Pressure::Warn,
                    Some(aterm_sysprobe::MemoryLevel::Critical) => Pressure::Critical,
                    None => Pressure::Unknown,
                },
            }),
            gpu: aterm_sysprobe::gpu().map(|g| GpuSample {
                device_pct: g.device_pct,
                cores: g.cores,
                name: g.name,
            }),
            volume: local_volume(
                &volume_path,
                &aterm_sysprobe::volumes::table(),
                aterm_sysprobe::volume,
            )
            .map(|v| VolumeSample {
                total: v.total,
                free: v.free,
                available: v.available,
                mount: v.mount.display().to_string(),
            }),
            procs: aterm_sysprobe::processes().map(|sweep| {
                sweep
                    .procs
                    .into_iter()
                    .map(|p| ProcSample {
                        pid: p.pid,
                        ppid: p.ppid,
                        cpu_ns: p.cpu_ns,
                        footprint: p.footprint,
                        name: p.name,
                    })
                    .collect()
            }),
            self_pid: req.self_pid,
            tabs: req.tabs.clone(),
        };
        self.vitals.absorb(&sample)
    }
}

/// Read only a mount root that a complete table calls local and not automounted.
/// The tab's directory is used for lexical lookup, never passed to the filesystem
/// reader: it may contain a symlink or lie on an unavailable network mount.
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
    /// Whether any window shows the Deck now, and whether aterm is in front.
    fn vitals_audience(&self) -> (bool, bool) {
        let shown = self.stats_bars_form() == crate::stats_deck::StatsBars::Deck
            && self.windows.values().any(|ws| {
                (ws.os_window.is_some() || crate::stats_deck::deck_on_headless()) && !ws.occluded
            });
        let focused = self.windows.values().any(|ws| ws.focused);
        (shown, focused)
    }

    /// The deadline the loop folds for the next reading (`None`: no window
    /// shows the Deck, so nothing is read).
    pub(crate) fn vitals_deadline(&self) -> Option<Instant> {
        self.vitals.next_due
    }

    /// A level judged every wake: adopt a snapshot the thread left, and when a
    /// reading is due, ask for the next one. A hidden Deck disarms.
    pub(crate) fn service_vitals(&mut self, now: Instant) {
        if self.vitals.adopt() {
            self.redraw_stats_decks();
        }
        let (shown, focused) = self.vitals_audience();
        if !shown {
            self.vitals.next_due = None;
            return;
        }
        if self.vitals.next_due.is_some_and(|due| due > now) {
            return;
        }
        let period = if focused {
            FOCUSED_PERIOD
        } else {
            BACKGROUND_PERIOD
        };
        self.vitals.next_due = Some(now + period);
        // A test App has no loop to post to, so it starts no thread.
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        let req = self.vitals_request();
        self.vitals.request(req, &proxy);
    }

    /// `Wake::Vitals`: the thread left a snapshot.
    pub(crate) fn on_vitals_wake(&mut self) {
        if self.vitals.adopt() {
            self.redraw_stats_decks();
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
        if cfg!(any(target_os = "macos", target_os = "linux")) {
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
