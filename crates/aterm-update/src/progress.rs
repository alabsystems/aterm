// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LIVE PROGRESS of the updater's own check, for a host that wants to SHOW it
//! (the aterm window's update status bar).
//!
//! The updater downloads a release container by shelling `curl` into
//! `Updates/download/<name>.part` and blocks until it exits, so no byte ever
//! passes through this process. A sibling thread stats the growing `.part` against the release
//! asset's DECLARED size and reports through one process-wide observer. The
//! observer is installed once by the host ([`set_progress_observer`]); every
//! lane that runs a check in this process (the background loop, a manual
//! "Check for Updates") reports through it, so the host never has to know which
//! thread is downloading.
//!
//! # What is (and is not) reported
//!
//! Only the phases a user would wait on: a download, the verify/stage that
//! follows it, and how that ended. A check that finds nothing to do — the
//! common case, every few minutes — reports NOTHING, so a host that reserves
//! screen space for a live report never moves for a routine check. Nor is a
//! sibling PROCESS's download visible here (a terminal session running its own
//! loop may win the cross-process stage lock): this is an in-process channel,
//! best-effort by design, and the durable record stays `status.toml`.
//!
//! # Honesty
//!
//! * `bytes_total` is the asset's size as the download host answers a HEAD
//!   for it ([`watch_download`]'s size probe, run BESIDE the download, never
//!   before it), and `0` until that answer lands or when it
//!   never does (offline, refused, a size past the download's own bound). A
//!   host renders `0` as "unknown", never divides by it. The size only draws
//!   the meter: what is installed is proved by the digest, as before.
//! * `bytes_done` is the `.part` file's size and can DROP: curl `--retry`
//!   truncates its sink between attempts. A host clamps and moves on.
//! * A report is emitted on a worker thread; the observer must be cheap and must
//!   never block (the host posts an event-loop message).

#[cfg(any(target_os = "macos", test))]
use std::path::Path;
#[cfg(any(target_os = "macos", test))]
use std::sync::Arc;
use std::sync::OnceLock;
#[cfg(any(target_os = "macos", test))]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(any(target_os = "macos", test))]
use std::time::Duration;

/// One report from inside a running check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    /// The release container is being fetched. `bytes_total == 0` ⇒ unknown.
    Downloading {
        version: String,
        bytes_done: u64,
        bytes_total: u64,
    },
    /// The container arrived: size/digest checks, extraction, codesign/Gatekeeper
    /// policy and the atomic stage publish — seconds each, one phase.
    Verifying { version: String },
    /// Verified and published: the in-session apply lane applies it in place
    /// (automatic by default, landing within `aterm-gui`'s `LANDS_WITHIN`, 15
    /// minutes); the next launch picks it up only if no handoff ever completes.
    Staged { version: String, build: u64 },
    /// The download was cut short by something that will heal on its own (the
    /// download host answering 429); the check backs off and retries later.
    Deferred { detail: String },
    /// A check that had begun downloading failed. `detail` is the same sentence
    /// `status.toml` records.
    Failed { detail: String },
    /// The release this check had begun downloading is WITHHELD on this machine —
    /// its signer was revoked, it was yanked, or it crash-looped here and was
    /// quarantined — while the download ran. No failure and nothing to retry: the
    /// next check refuses it before any download. `detail` is the `held: …` sentence
    /// `status.toml` records.
    Withheld { detail: String },
}

/// The host's observer: called on the updater's worker threads, must not block.
pub type ProgressNotify = Box<dyn Fn(Progress) + Send + Sync>;

static OBSERVER: OnceLock<ProgressNotify> = OnceLock::new();

/// Whether the CURRENT check has reported a download — the wrapper's gate for
/// turning an `Err` into a [`Progress::Failed`] report (a check that never got
/// as far as bytes has nothing on screen to answer for). One check runs at a
/// time per process (`check_lane`), so a process-wide flag is exact.
#[cfg(any(target_os = "macos", test))]
static DOWNLOAD_BEGAN: AtomicBool = AtomicBool::new(false);

/// Install the process-wide observer. First caller wins (the host installs it
/// once at startup, before any check can run); later calls are ignored.
pub fn set_progress_observer(observer: ProgressNotify) {
    let _ = OBSERVER.set(observer);
}

/// Report one step: the heartbeat of a check in flight, and the host's observer
/// when one is installed (the window; a terminal session has none).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn report(p: Progress) {
    if let Progress::Downloading { .. } = &p {
        DOWNLOAD_BEGAN.store(true, Ordering::Relaxed);
    }
    // A check that is moving bytes is alive: a container on a slow link
    // can take hours inside one check, and without this stamp the watchdog would
    // judge that honest download against a budget sized for everything else a
    // check does. Only a check in flight is re-stamped (`beat_progress`), so a
    // manual check's download never vouches for a checker that is stuck elsewhere.
    if matches!(p, Progress::Downloading { .. } | Progress::Verifying { .. }) {
        crate::checker_watch::WATCH.beat_progress();
    }
    if let Some(cb) = OBSERVER.get() {
        cb(p);
    }
}

/// Whether a download was reported since the last [`take_download_began`] —
/// consumed by the check wrapper at each check's end.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn take_download_began() -> bool {
    DOWNLOAD_BEGAN.swap(false, Ordering::Relaxed)
}

/// Watch a growing `<dest>` (the curl sink) at 10 Hz while the download of
/// `url` runs, reporting [`Progress::Downloading`] on every change of its size
/// or of the known total; the guard stops and joins the poller on drop.
///
/// The first report (0 of an unknown total) is emitted synchronously, so a host sees
/// the download begin even if curl finishes before the first poll. The total comes
/// from [`asset_size`], a HEAD of the same asset run on its own thread BESIDE the
/// download — the download never waits for it, and a probe still in flight when the
/// download ends is simply not waited for.
///
/// THE POLLER RUNS WITH NO OBSERVER TOO: its reports re-stamp the checker's heartbeat
/// while bytes move ([`report`]), and both machine-wide locks' holder beats live off
/// that heartbeat, so a terminal session's slow download must not read as a stopped
/// holder. Only the size probe, which draws nothing without a host, is left out.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub(crate) fn watch_download(dest: &Path, version: &str, url: &str) -> DownloadWatch {
    if OBSERVER.get().is_none() {
        return watch_with(dest, version, || None, Arc::new(report));
    }
    let url = url.to_string();
    watch_with(dest, version, move || asset_size(&url), Arc::new(report))
}

/// The asset's size from an anonymous HEAD that follows the storage redirect
/// ([`aterm_update_core::vendor_content_length`]: https on every hop, bounded,
/// never a credential), admitted only inside the download's own bound
/// ([`aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND`]). `None` when the host
/// does not say, which leaves the meter busy.
#[cfg(any(target_os = "macos", test))]
fn asset_size(url: &str) -> Option<u64> {
    aterm_update_core::vendor_content_length(url)
        .ok()
        .filter(|n| (1..=aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND).contains(n))
}

/// [`watch_download`]'s body over any size probe and sink, for the tests.
#[cfg(any(target_os = "macos", test))]
fn watch_with(
    dest: &Path,
    version: &str,
    size: impl FnOnce() -> Option<u64> + Send + 'static,
    sink: Arc<dyn Fn(Progress) + Send + Sync>,
) -> DownloadWatch {
    sink(Progress::Downloading {
        version: version.to_string(),
        bytes_done: 0,
        bytes_total: 0,
    });
    // The total, once the probe answers; 0 until then. The probe thread is
    // detached on purpose: it writes this atomic and nothing else.
    let total = Arc::new(AtomicU64::new(0));
    let total_w = Arc::clone(&total);
    let _ = std::thread::Builder::new()
        .name("aterm-update-size-probe".into())
        .spawn(move || {
            if let Some(n) = size() {
                total_w.store(n, Ordering::Release);
            }
        });
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = Arc::clone(&stop);
    let dest = dest.to_path_buf();
    let version = version.to_string();
    let handle = std::thread::Builder::new()
        .name("aterm-update-part-poll".into())
        .spawn(move || {
            let mut last: Option<(u64, u64)> = None;
            while !stop2.load(Ordering::Acquire) {
                // Regular files only: the sink is ours, but a symlink planted at
                // the path is not a byte count worth reporting.
                let len = std::fs::symlink_metadata(&dest)
                    .ok()
                    .filter(|m| m.file_type().is_file())
                    .map(|m| m.len());
                let now = (len.unwrap_or(0), total.load(Ordering::Acquire));
                if (len.is_some() || now.1 > 0) && last != Some(now) {
                    last = Some(now);
                    sink(Progress::Downloading {
                        version: version.clone(),
                        bytes_done: now.0,
                        bytes_total: now.1,
                    });
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })
        .ok();
    DownloadWatch { stop, handle }
}

/// Stops the `.part` poller on drop.
#[cfg(any(target_os = "macos", test))]
pub(crate) struct DownloadWatch {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(any(target_os = "macos", test))]
impl Drop for DownloadWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The process-wide download flag and heartbeat are shared: the tests that
    /// report through [`report`] take turns.
    static GLOBAL_REPORTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The wrapper's gate: a download report arms it, taking it disarms it, and
    /// a report that is not a download leaves it alone.
    #[test]
    fn the_download_flag_is_set_by_a_download_report_and_consumed_once() {
        let _turn = GLOBAL_REPORTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = take_download_began();
        report(Progress::Verifying {
            version: "0.1.0".into(),
        });
        assert!(!take_download_began(), "verifying alone is not a download");
        report(Progress::Downloading {
            version: "0.1.0".into(),
            bytes_done: 1,
            bytes_total: 2,
        });
        assert!(take_download_began());
        assert!(!take_download_began(), "consumed");
    }

    /// A DOWNLOAD IN A TERMINAL SESSION KEEPS ITS CHECKER ALIVE.
    /// No observer is installed in this test process — the session's shape — and
    /// watching used to spawn nothing, so the heartbeat of a check that was
    /// downloading never moved: past 65 minutes on a slow link it read stalled, and
    /// the window booked the session as a stopped stage holder. Now every byte the
    /// sink gains re-stamps a check in flight. The minute is a hang detector.
    #[test]
    fn a_download_with_no_observer_still_restamps_the_checking_heartbeat() {
        use crate::checker_watch::{CheckerPhase, WATCH};
        let _turn = GLOBAL_REPORTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            OBSERVER.get().is_none(),
            "PRECONDITION: the session's shape"
        );
        let dir = std::env::temp_dir().join(format!("aterm-update-beat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join("aterm.zip.part");
        let generation = WATCH.register(1_000);
        assert!(WATCH.beat(generation, CheckerPhase::Checking));
        let stamped = WATCH.snapshot().unwrap().at_secs;
        let watch = watch_download(&part, "0.1.0", "https://example.invalid/aterm.zip");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let mut bytes = 0;
        let moved = loop {
            let beat = WATCH.snapshot().unwrap();
            if beat.generation == generation && beat.at_secs > stamped {
                break true;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            bytes += 4096;
            std::fs::write(&part, vec![0u8; bytes]).unwrap();
            std::thread::sleep(Duration::from_millis(100));
        };
        drop(watch);
        let _ = WATCH.supersede(generation);
        let _ = take_download_began();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            moved,
            "the bytes a session downloads must keep its checker's heartbeat fresh"
        );
    }

    /// Every report a sink saw.
    type Seen = Arc<std::sync::Mutex<Vec<Progress>>>;
    /// A sink the watcher reports into.
    type Sink = Arc<dyn Fn(Progress) + Send + Sync>;

    /// Every report the sink saw, and a sink that records into it.
    fn recorder() -> (Seen, Sink) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let into = Arc::clone(&seen);
        (seen, Arc::new(move |p| into.lock().unwrap().push(p)))
    }

    /// Wait until a report matches — a report that must come, so a minute's
    /// hang detector, never a latency budget.
    fn wait_for(seen: &std::sync::Mutex<Vec<Progress>>, want: &Progress) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            if seen.lock().unwrap().contains(want) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        seen.lock().unwrap().contains(want)
    }

    /// THE SIZE MAKES THE FILL (design ruling 226): the probe's total rides
    /// every report from the moment it lands, beside the `.part`'s bytes, so a
    /// person's download row has a fill and an ETA, not only a comet.
    #[test]
    fn the_probed_size_rides_every_report_once_it_lands() {
        let dir = std::env::temp_dir().join(format!("aterm-update-size-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join("aterm.zip.part");
        let (seen, sink) = recorder();
        let w = watch_with(&part, "0.95.0", || Some(74_000_000), sink);
        let first = Progress::Downloading {
            version: "0.95.0".into(),
            bytes_done: 0,
            bytes_total: 0,
        };
        assert_eq!(seen.lock().unwrap().first(), Some(&first), "at once");
        // The total lands before any byte: reported with no bytes yet.
        assert!(wait_for(
            &seen,
            &Progress::Downloading {
                version: "0.95.0".into(),
                bytes_done: 0,
                bytes_total: 74_000_000,
            }
        ));
        std::fs::write(&part, vec![0u8; 4096]).unwrap();
        assert!(wait_for(
            &seen,
            &Progress::Downloading {
                version: "0.95.0".into(),
                bytes_done: 4096,
                bytes_total: 74_000_000,
            }
        ));
        drop(w);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A host that does not say keeps the total unknown (the busy comet), and
    /// a probe still waiting on the network never holds the download's end: the
    /// probe here answers only once the test lets it, AFTER the watch has
    /// dropped, so a drop that waited for it could not return before its gate's
    /// two minutes ran out — and the probe is seen still waiting when the drop
    /// is back.
    #[test]
    fn a_silent_or_slow_size_probe_leaves_the_total_unknown() {
        let dir = std::env::temp_dir().join(format!("aterm-update-slow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join("aterm.zip.part");
        std::fs::write(&part, vec![0u8; 100]).unwrap();
        let (seen, sink) = recorder();
        let w = watch_with(&part, "0.95.0", || None, sink);
        assert!(wait_for(
            &seen,
            &Progress::Downloading {
                version: "0.95.0".into(),
                bytes_done: 100,
                bytes_total: 0,
            }
        ));
        drop(w);
        let (_, sink) = recorder();
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let answered = Arc::new(AtomicBool::new(false));
        let probe_answered = Arc::clone(&answered);
        let slow = watch_with(
            &part,
            "0.95.0",
            move || {
                let _ = gate.recv_timeout(Duration::from_secs(120));
                probe_answered.store(true, Ordering::Release);
                Some(1)
            },
            sink,
        );
        drop(slow);
        assert!(
            !answered.load(Ordering::Acquire),
            "the download's end never waits for the probe"
        );
        let _ = release.send(());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
