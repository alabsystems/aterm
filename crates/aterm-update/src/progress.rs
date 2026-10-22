// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live progress of the updater's own check, for a host that shows it (the window's update
//! status bar).
//!
//! The download shells `curl` into `Updates/download/<name>.part`, so no byte passes through
//! this process: a sibling thread stats the growing `.part` and reports through one
//! process-wide observer the host installs once ([`set_progress_observer`]). Every lane that
//! runs a check in this process reports through it.
//!
//! # What is (and is not) reported
//!
//! * `bytes_total` is the asset's size from a HEAD run beside the download, `0` until it
//!   lands or when it never does; a host renders `0` as unknown. The digest, not this size,
//!   proves what is installed.
//! * `bytes_done` is the `.part` size and can drop: curl `--retry` truncates its sink between
//!   attempts.
//! * Reports arrive on a worker thread; the observer must be cheap and never block.

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
    /// minutes; on a press while `[update] auto_apply` is off). A stage no handoff
    /// completed stays for the next launch's in-session lane; that launch's boot swaps it
    /// only if it is a replaceable one (`ImageReplacement::Replaceable`), or a window's
    /// with the automatic lane on once `LEFT_STAGE_ESCAPE` launches that left it ended.
    Staged { version: String, build: u64 },
    /// The download was cut short by something that will heal on its own (the
    /// download host answering 429); the check backs off and retries later.
    Deferred { detail: String },
    /// A check that had begun downloading failed. `detail` is the same sentence
    /// `status.toml` records.
    Failed { detail: String },
    /// The release being downloaded was withheld on this machine while the download ran (signer
    /// revoked, yanked, or quarantined after crash-looping here). Nothing to retry: the next
    /// check refuses it before downloading. `detail` is the `held: …` sentence `status.toml`
    /// records.
    Withheld { detail: String },
}

/// The host's observer: called on the updater's worker threads, must not block.
pub type ProgressNotify = Box<dyn Fn(Progress) + Send + Sync>;

static OBSERVER: OnceLock<ProgressNotify> = OnceLock::new();

/// Whether the current check has reported a download: only then does the wrapper turn an `Err`
/// into a [`Progress::Failed`] report. One check runs at a time per process (`check_lane`), so
/// a process-wide flag is exact.
#[cfg(any(target_os = "macos", test))]
static DOWNLOAD_BEGAN: AtomicBool = AtomicBool::new(false);

/// Install the process-wide observer. First caller wins (the host installs it
/// once at startup, before any check can run); later calls are ignored.
pub fn set_progress_observer(observer: ProgressNotify) {
    let _ = OBSERVER.set(observer);
}

/// Report one step: download and verify steps re-stamp the heartbeat of a check in flight, and
/// the host's observer, if installed, gets every step (a terminal session has none).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn report(p: Progress) {
    if let Progress::Downloading { .. } = &p {
        DOWNLOAD_BEGAN.store(true, Ordering::Relaxed);
    }
    // A check moving bytes is alive: a slow-link download can take hours, past the watchdog's
    // budget for everything else a check does. Only a check in flight is re-stamped
    // (`beat_progress`), so a manual check's download never vouches for a stuck checker.
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

/// Watch the growing `dest` (the curl sink) at 10 Hz while `url` downloads, reporting
/// [`Progress::Downloading`] whenever its size or the known total changes; the guard stops and
/// joins the poller on drop.
///
/// The first report (0 of unknown) is synchronous, so a host sees the download begin even if
/// curl finishes before the first poll. The total comes from [`asset_size`], a HEAD on its own
/// thread beside the download, never waited for.
///
/// The poller runs with no observer too: its reports re-stamp the checker's heartbeat
/// ([`report`]), which both machine-wide locks' holder beats live off, so a terminal session's
/// slow download does not read as a stopped holder. Only the size probe is skipped then.
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

    /// A download in a terminal session (no observer installed) still re-stamps
    /// the checker's heartbeat as `.part` grows so a slow download is not
    /// mistaken for a stalled checker.
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

    /// Wait until a report matches; the minute is a hang detector.
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

    /// The size probe's total rides every report from the moment it lands,
    /// beside the `.part`'s bytes.
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

    /// A silent probe leaves the total unknown, and dropping the watch never waits for a probe
    /// still in flight.
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
