// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live progress observer for updater checks.

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
    /// The release container is being fetched.
    Downloading {
        version: String,
        bytes_done: u64,
        bytes_total: u64,
    },
    /// The container arrived: size/digest checks, extraction, codesign/Gatekeeper policy and the
    /// atomic stage publish — seconds each, one phase.
    Verifying { version: String },
    /// Verified and published: the in-session apply lane applies it in place (automatic by default,
    /// landing within `aterm-gui`'s `LANDS_WITHIN`, 55 seconds; on a press while `[update]
    /// auto_apply` is off).
    Staged { version: String, build: u64 },
    /// The download was cut short by something that will heal on its own (the download host
    /// answering 429); the check backs off and retries later.
    Deferred { detail: String },
    /// A check that had begun downloading failed.
    Failed { detail: String },
    /// The release being downloaded was withheld on this machine while the download ran (signer
    /// revoked, yanked, or quarantined after crash-looping here).
    Withheld { detail: String },
}

/// A progress event with the checker generation that produced it, when any.
#[derive(Clone, Debug)]
pub struct ProgressReport {
    /// The reported step.
    pub progress: Progress,
    /// The background checker that produced this event; manual checks have none.
    pub checker_generation: Option<u64>,
}

impl ProgressReport {
    /// Whether this event still belongs to the current checker when the host consumes it.
    #[must_use]
    pub fn is_current_for(&self, watch: &crate::checker_watch::CheckerWatch) -> bool {
        self.checker_generation
            .is_none_or(|generation| generation != 0 && watch.is_current(generation))
    }
}

/// The host's observer: called on the updater's worker threads, must not block.
pub type ProgressNotify = Box<dyn Fn(ProgressReport) + Send + Sync>;

static OBSERVER: OnceLock<ProgressNotify> = OnceLock::new();

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy)]
struct CheckerReporter {
    watch: &'static crate::checker_watch::CheckerWatch,
    generation: u64,
}

#[cfg(any(target_os = "macos", test))]
std::thread_local! {
    static CHECKER_REPORTER: std::cell::Cell<Option<CheckerReporter>> = const { std::cell::Cell::new(None) };
}

/// Bind reports on this thread to one checker; download pollers capture the same owner.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn checker_scope(
    watch: &'static crate::checker_watch::CheckerWatch,
    generation: u64,
) -> CheckerScope {
    CheckerScope {
        previous: CHECKER_REPORTER.replace(Some(CheckerReporter { watch, generation })),
        thread: std::marker::PhantomData,
    }
}

#[cfg(any(target_os = "macos", test))]
#[must_use]
pub(crate) struct CheckerScope {
    previous: Option<CheckerReporter>,
    thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(any(target_os = "macos", test))]
impl Drop for CheckerScope {
    fn drop(&mut self) {
        CHECKER_REPORTER.set(self.previous);
    }
}

/// Whether one check began downloading its release's container. Each check owns its own, so a
/// check (or a test of the binary) that ends meanwhile cannot take it, and every check reports
/// its own failure.
#[cfg(target_os = "macos")]
#[derive(Debug, Default)]
pub(crate) struct DownloadBegan(std::cell::Cell<bool>);

#[cfg(target_os = "macos")]
impl DownloadBegan {
    /// The check began downloading.
    pub(crate) fn mark(&self) {
        self.0.set(true);
    }

    /// Whether the check began downloading.
    pub(crate) fn began(&self) -> bool {
        self.0.get()
    }
}

/// Install the process-wide observer.
pub fn set_progress_observer(observer: ProgressNotify) {
    let _ = OBSERVER.set(observer);
}

/// Report one step for this thread's checker and notify the host's observer, if installed.
#[cfg(target_os = "macos")]
pub(crate) fn report(p: Progress) {
    report_for(CHECKER_REPORTER.get(), p);
}

#[cfg(any(target_os = "macos", test))]
fn report_for(checker: Option<CheckerReporter>, p: Progress) -> bool {
    let stamped = matches!(p, Progress::Downloading { .. } | Progress::Verifying { .. })
        && checker.is_some_and(|owner| owner.watch.beat_progress(owner.generation));
    if let Some(cb) = OBSERVER.get() {
        cb(ProgressReport {
            progress: p,
            checker_generation: checker.map(|owner| owner.generation),
        });
    }
    stamped
}

/// Watch the growing `dest` (the curl sink) at 10 Hz while `url` downloads, reporting
/// [`Progress::Downloading`] whenever its size or the known total changes; the guard stops and
/// joins the poller on drop.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub(crate) fn watch_download(dest: &Path, version: &str, url: &str) -> DownloadWatch {
    let checker = CHECKER_REPORTER.get();
    let sink = Arc::new(move |p| {
        report_for(checker, p);
    });
    if OBSERVER.get().is_none() {
        return watch_with(dest, version, || None, sink);
    }
    let url = url.to_string();
    watch_with(dest, version, move || asset_size(&url), sink)
}

#[cfg(any(target_os = "macos", test))]
fn asset_size(url: &str) -> Option<u64> {
    aterm_update_core::vendor_content_length(url)
        .ok()
        .filter(|n| (1..=aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND).contains(n))
}

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
#[path = "progress_tests.rs"]
mod tests;
