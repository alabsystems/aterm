// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LIVE PROGRESS of the updater's own check, for a host that wants to SHOW it
//! (the aterm window's update status bar).
//!
//! The updater downloads a release container by shelling `curl` into
//! `Updates/download/<name>.part` and blocks until it exits, so no byte ever
//! passes through this process — exactly atpkg's situation, and the answer is
//! the same: a sibling thread stats the growing `.part` against the release
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
//! * `bytes_total` is the GitHub API's asset size, `0` when it was not reported —
//!   a host must render that as "unknown", never divide by it.
//! * `bytes_done` is the `.part` file's size and can DROP: curl `--retry`
//!   truncates its sink between attempts. A host clamps and moves on.
//! * A report is emitted on a worker thread; the observer must be cheap and must
//!   never block (the host posts an event-loop message).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
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
    /// (automatic by default, forced within ~2 min); the next launch picks it up
    /// only if no handoff ever completes.
    Staged { version: String, build: u64 },
    /// The download was cut short by something that will heal on its own (a
    /// GitHub rate limit); the check backs off and retries later.
    Deferred { detail: String },
    /// A check that had begun downloading failed. `detail` is the same sentence
    /// `status.toml` records.
    Failed { detail: String },
}

/// The host's observer: called on the updater's worker threads, must not block.
pub type ProgressNotify = Box<dyn Fn(Progress) + Send + Sync>;

static OBSERVER: OnceLock<ProgressNotify> = OnceLock::new();

/// Whether the CURRENT check has reported a download — the wrapper's gate for
/// turning an `Err` into a [`Progress::Failed`] report (a check that never got
/// as far as bytes has nothing on screen to answer for). One check runs at a
/// time per process (`check_lane`), so a process-wide flag is exact.
static DOWNLOAD_BEGAN: AtomicBool = AtomicBool::new(false);

/// Install the process-wide observer. First caller wins (the host installs it
/// once at startup, before any check can run); later calls are ignored.
pub fn set_progress_observer(observer: ProgressNotify) {
    let _ = OBSERVER.set(observer);
}

/// Report one step. A no-op with no observer installed (every non-GUI process).
pub(crate) fn report(p: Progress) {
    if let Progress::Downloading { .. } = &p {
        DOWNLOAD_BEGAN.store(true, Ordering::Relaxed);
    }
    if let Some(cb) = OBSERVER.get() {
        cb(p);
    }
}

/// Whether a download was reported since the last [`take_download_began`] —
/// consumed by the check wrapper at each check's end.
pub(crate) fn take_download_began() -> bool {
    DOWNLOAD_BEGAN.swap(false, Ordering::Relaxed)
}

/// Watch a growing `<dest>` (the curl sink) at 10 Hz while the download runs,
/// reporting [`Progress::Downloading`] on every size change; the guard stops and
/// joins the poller on drop. With no observer installed nothing is spawned.
///
/// The first report (0 of `total`) is emitted synchronously, so a host sees the
/// download begin even if curl finishes before the first poll.
#[must_use]
pub(crate) fn watch_download(dest: &Path, version: &str, total: u64) -> DownloadWatch {
    let inert = DownloadWatch {
        stop: Arc::new(AtomicBool::new(true)),
        handle: None,
    };
    if OBSERVER.get().is_none() {
        return inert;
    }
    report(Progress::Downloading {
        version: version.to_string(),
        bytes_done: 0,
        bytes_total: total,
    });
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = Arc::clone(&stop);
    let dest = dest.to_path_buf();
    let version = version.to_string();
    let handle = std::thread::Builder::new()
        .name("aterm-update-part-poll".into())
        .spawn(move || {
            let mut last: Option<u64> = None;
            while !stop2.load(Ordering::Acquire) {
                // Regular files only: the sink is ours, but a symlink planted at
                // the path is not a byte count worth reporting.
                let len = std::fs::symlink_metadata(&dest)
                    .ok()
                    .filter(|m| m.file_type().is_file())
                    .map(|m| m.len());
                if let Some(n) = len
                    && last != Some(n)
                {
                    last = Some(n);
                    report(Progress::Downloading {
                        version: version.clone(),
                        bytes_done: n,
                        bytes_total: total,
                    });
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })
        .ok();
    DownloadWatch { stop, handle }
}

/// Stops the `.part` poller on drop.
pub(crate) struct DownloadWatch {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

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

    /// The wrapper's gate: a download report arms it, taking it disarms it, and
    /// a report that is not a download leaves it alone.
    #[test]
    fn the_download_flag_is_set_by_a_download_report_and_consumed_once() {
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

    /// With no observer, watching costs nothing: no thread, no reports.
    #[test]
    fn watching_without_an_observer_spawns_nothing() {
        let dir = std::env::temp_dir();
        let w = watch_download(&dir.join("never-created.part"), "0.1.0", 10);
        assert!(w.handle.is_none());
        assert!(w.stop.load(Ordering::Acquire));
    }
}
