// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live config hot-reload watcher.
//!
//! A single background thread observes the user config file
//! (`config_path()` — `$XDG_CONFIG_HOME/aterm/aterm.toml`, else
//! `~/.config/aterm/aterm.toml`) on a fixed cadence and, when its
//! bounded content/target generation changes, posts a
//! [`Wake::ConfigReloadObserved`](crate::Wake) with those exact admitted bytes;
//! the UI thread never reopens the pathname and only applies the immutable
//! observation (see `App::reload_config_observation`). The same worker fingerprints the bounded user-theme
//! directory; when it changes, the worker parses a complete
//! [`ThemeCatalog`](crate::app_config::ThemeCatalog)
//! and hands only that immutable result to the event loop.
//!
//! WHY poll, not a filesystem-notification crate: this codebase is
//! hardened, dependency-conscious, and sandbox-sensitive (the `Containment`
//! mode denies reads/writes under `~/.config/aterm`). A ~500 ms bounded
//! observation loop adds ZERO new dependencies, no inotify/FSEvents/kqueue file
//! descriptors, and no surprising behavior under a sandbox profile. Config
//! failures are posted as typed, de-duplicated status edges while the previous
//! live generation remains active. Recovery posts a matching clear edge. Theme
//! polling is metadata-only while idle: bounded path/file identity, mtime, and
//! ctime (where the platform exposes it) select a candidate edge; theme bytes are
//! read only while preparing that changed generation. A timestamp-preserving
//! rewrite or atomic replacement therefore cannot leave the process stale on
//! macOS/Linux without continuously hashing tens of MiB of theme data.
//!
//! Two hard rules borrowed from the notification/clipboard threads:
//!
//! 1. **Never block the UI thread.** The watcher performs bounded file
//!    observation and theme discovery itself, then posts immutable values with
//!    `EventLoopProxy::send_event`; the UI thread only validates/reduces memory
//!    and remains the sole owner of the renderer, window, and per-tab engines.
//! 2. **Self-terminating.** When the proxy `send_event` fails (the event loop is
//!    gone — the app is exiting), the loop breaks and the thread ends. It also
//!    exits as soon as the proxy can't be reached, so it never outlives the app.

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use winit::event_loop::EventLoopProxy;

use crate::Wake;

/// How often the watcher observes the config file. 500 ms is imperceptible for
/// an interactive "save and see it apply" loop. Healthy idle polls read only
/// bounded metadata. Candidate-edge reads share Manual's 512-KiB cap, and the
/// thread is parked in `sleep` between polls.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// The independently recoverable host inputs watched by this worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum WatchTarget {
    Config,
    Themes,
}

/// Bounded user-facing failure classes. Raw host error strings are neither a
/// stable de-duplication key nor suitable UI: they may contain private paths and
/// can vary between identical retries. The worker reduces host errors into this
/// closed vocabulary before crossing the event-loop boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum WatchFailureKind {
    ConfigUnreadable,
    ConfigInvalidUtf8,
    ConfigInvalidToml,
    ConfigTooLarge,
    ConfigNotRegular,
    ConfigChangedWhileReading,
    ConfigUnsafeBinding,
    ConfigPreparationFailed,
    ThemeDirectoryUnreadable,
    ThemeEntryUnreadable,
    ThemeFileUnavailable,
    ThemeChangedWhileScanning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct WatchFailure {
    pub(crate) target: WatchTarget,
    pub(crate) kind: WatchFailureKind,
}

/// One edge in the failure/recovery protocol. A stable failure is emitted once;
/// a successful observation after it emits exactly one `Recovered` edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WatchStatusEvent {
    Failed(WatchFailure),
    Recovered(WatchTarget),
}

/// Process-global projection installed by the UI reducer. Settings and Manual
/// each retain this as a separate status field, so ordinary action feedback and
/// dirty-buffer recovery are never overwritten by a watcher.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WatchStatusState {
    config: Option<WatchFailureKind>,
    themes: Option<WatchFailureKind>,
    /// Latest exact readable generation handed to the asynchronous config
    /// preparation/admission lane. Only admission of this baseline may clear a
    /// config failure; an older completion cannot recover a newer error.
    config_candidate: Option<crate::native_document_host::AtomicFileBaseline>,
}

impl WatchStatusState {
    #[must_use]
    pub(crate) fn has_config_candidate(&self) -> bool {
        self.config_candidate.is_some()
    }

    #[must_use]
    pub(crate) fn reduce(&mut self, event: WatchStatusEvent) -> bool {
        let (target, next) = match event {
            WatchStatusEvent::Failed(failure) => (failure.target, Some(failure.kind)),
            WatchStatusEvent::Recovered(target) => (target, None),
        };
        let slot = match target {
            WatchTarget::Config => {
                if next.is_some() {
                    self.config_candidate = None;
                }
                &mut self.config
            }
            WatchTarget::Themes => &mut self.themes,
        };
        if *slot == next {
            return false;
        }
        *slot = next;
        true
    }

    pub(crate) fn note_config_candidate(
        &mut self,
        baseline: crate::native_document_host::AtomicFileBaseline,
    ) {
        self.config_candidate = Some(baseline);
    }

    /// Admit recovery only for the newest exact watcher generation. Returns
    /// whether visible status changed; consuming a healthy candidate with no
    /// prior failure is intentionally presentation-inert.
    pub(crate) fn acknowledge_config_candidate(
        &mut self,
        baseline: &crate::native_document_host::AtomicFileBaseline,
    ) -> bool {
        if self.config_candidate.as_ref() != Some(baseline) {
            return false;
        }
        self.config_candidate = None;
        self.reduce(WatchStatusEvent::Recovered(WatchTarget::Config))
    }

    /// Reject only the exact watcher generation that is still current. A slow
    /// failure from generation A must neither invalidate generation B's recovery
    /// ticket nor replace B's eventual status after B has already been observed.
    pub(crate) fn reject_config_candidate(
        &mut self,
        baseline: &crate::native_document_host::AtomicFileBaseline,
        kind: WatchFailureKind,
    ) -> bool {
        if self.config_candidate.as_ref() != Some(baseline) {
            return false;
        }
        self.reduce(WatchStatusEvent::Failed(WatchFailure {
            target: WatchTarget::Config,
            kind,
        }))
    }

    pub(crate) fn message(&self) -> Option<String> {
        let mut messages = Vec::with_capacity(2);
        if let Some(kind) = self.config {
            messages.push(failure_message(kind));
        }
        if let Some(kind) = self.themes {
            messages.push(failure_message(kind));
        }
        (!messages.is_empty()).then(|| messages.join(" "))
    }
}

fn failure_message(kind: WatchFailureKind) -> &'static str {
    match kind {
        WatchFailureKind::ConfigUnreadable => {
            "aterm.toml reload rejected: the file could not be read. Running settings and Manual content were kept unchanged."
        }
        WatchFailureKind::ConfigInvalidUtf8 => {
            "aterm.toml reload rejected: the file is not valid UTF-8. Running settings and Manual content were kept unchanged."
        }
        WatchFailureKind::ConfigInvalidToml => {
            "aterm.toml reload rejected: the file is not a valid aterm configuration. Fix it in Manual; the running settings remain active."
        }
        WatchFailureKind::ConfigTooLarge => {
            "aterm.toml reload rejected: the file exceeds the 512 KiB limit. Running settings and Manual content were kept unchanged."
        }
        WatchFailureKind::ConfigNotRegular => {
            "aterm.toml reload rejected: the target is not a regular file. Running settings and Manual content were kept unchanged."
        }
        WatchFailureKind::ConfigChangedWhileReading => {
            "aterm.toml reload deferred: the file changed while it was being read. The watcher will retry and the running settings remain active."
        }
        WatchFailureKind::ConfigUnsafeBinding => {
            "aterm.toml reload rejected: its file binding is unsafe or changed. Reopen Manual after correcting the file."
        }
        WatchFailureKind::ConfigPreparationFailed => {
            "aterm.toml reload rejected: referenced assets or fonts could not be prepared safely. The running settings remain active."
        }
        WatchFailureKind::ThemeDirectoryUnreadable => {
            "Custom themes could not be refreshed because the theme directory is unreadable. The last valid theme catalog remains active."
        }
        WatchFailureKind::ThemeEntryUnreadable => {
            "Custom themes could not be refreshed because the directory scan was incomplete. The last valid theme catalog remains active."
        }
        WatchFailureKind::ThemeFileUnavailable => {
            "Custom themes could not be refreshed because a theme file changed or became unavailable while it was being read. The last valid theme catalog remains active."
        }
        WatchFailureKind::ThemeChangedWhileScanning => {
            "Custom theme reload was deferred because the directory kept changing. The watcher will retry and the last valid catalog remains active."
        }
    }
}

/// Worker-side failure latch. `transition` is the sole constructor of status
/// wakes, making repeated identical polls presentation-inert by construction.
#[derive(Default)]
struct FailureLatch(Option<WatchFailureKind>);

impl FailureLatch {
    fn transition(
        &mut self,
        target: WatchTarget,
        failure: Option<WatchFailureKind>,
    ) -> Option<WatchStatusEvent> {
        if self.0 == failure {
            return None;
        }
        let previous = self.0;
        self.0 = failure;
        match failure {
            Some(kind) => Some(WatchStatusEvent::Failed(WatchFailure { target, kind })),
            None if previous.is_some() => Some(WatchStatusEvent::Recovered(target)),
            None => None,
        }
    }

    fn clear_silently(&mut self) {
        self.0 = None;
    }
}

/// Spawn the single, process-wide config-watcher thread.
///
/// `path` is the resolved config path (from `config_path()`); `None` means there
/// is no config path at all (no `$XDG_CONFIG_HOME` and no `$HOME`), in which case
/// we spawn nothing — there is nothing to watch, and the no-config startup path
/// stays byte-identical. The thread captures `proxy` (an [`EventLoopProxy`]) and
/// posts [`Wake::ConfigReloadObserved`] on every admitted content/target change.
///
/// `startup_config_baseline` is the exact generation admitted earlier in startup.
/// The worker compares its first observation against that baseline, so an edit in
/// the load→watch handoff is delivered immediately while an unchanged launch
/// config is not redundantly re-applied.
pub fn spawn(
    path: Option<std::path::PathBuf>,
    startup_config_baseline: Option<crate::native_document_host::AtomicFileBaseline>,
    initial_themes: Arc<crate::app_config::ThemeCatalog>,
    proxy: EventLoopProxy<Wake>,
) {
    let theme_dir = aterm_types::scheme::user_theme_dir();
    if path.is_none() && theme_dir.is_none() {
        return;
    }
    std::thread::spawn(move || {
        // Baselines are sampled once on the worker. A later success after a
        // failed sample remains an edge even when its recovered bytes match the
        // pre-failure generation, so failure and generation state stay separate.
        // Stamp before the exact read: a mutation during this handoff therefore
        // remains an edge on the first poll instead of becoming the baseline.
        let mut last_config_stamp = path
            .as_deref()
            .and_then(|path| config_path_stamp(path).ok());
        let initial_config = path.as_deref().map(config_file_observation);
        let mut config_needs_retry = initial_config.as_ref().is_some_and(Result::is_err);
        let mut config_failure = FailureLatch::default();
        if let Some(Err(error)) = initial_config.as_ref()
            && !post_status(
                &proxy,
                &mut config_failure,
                WatchTarget::Config,
                Some(config_failure_kind(error)),
            )
        {
            return;
        }
        if let Some(Ok(observation)) = initial_config
            && initial_config_observation_changed(startup_config_baseline.as_ref(), &observation)
            && proxy
                .send_event(Wake::ConfigReloadObserved(observation))
                .is_err()
        {
            return;
        }

        let initial_theme_stamp = theme_dir.as_deref().map(theme_directory_stamp);
        // Theme discovery happened before this worker was spawned and has no
        // persisted file baseline. Treat the first successful poll as a
        // reconciliation edge so an edit in that startup gap cannot be lost.
        let mut last_theme_stamp = None;
        let mut theme_failure = FailureLatch::default();
        if let Some(Err(kind)) = initial_theme_stamp.as_ref()
            && !post_status(&proxy, &mut theme_failure, WatchTarget::Themes, Some(*kind))
        {
            return;
        }
        let mut themes = initial_themes;
        loop {
            std::thread::sleep(POLL_INTERVAL);
            if let Some(path) = path.as_deref() {
                match config_path_stamp(path) {
                    Err(kind) => {
                        config_needs_retry = true;
                        if !post_status(
                            &proxy,
                            &mut config_failure,
                            WatchTarget::Config,
                            Some(kind),
                        ) {
                            break;
                        }
                    }
                    Ok(stamp) => {
                        let candidate = prepare_config_edge_with(
                            last_config_stamp.as_ref(),
                            stamp,
                            config_needs_retry,
                            || config_file_observation(path),
                        );
                        if let Some((stamp, observation)) = candidate {
                            match observation {
                                Ok(observation) => {
                                    // Acknowledge only after the exact sampled generation is
                                    // enqueued. The UI never races a second pathname read.
                                    if proxy
                                        .send_event(Wake::ConfigReloadObserved(observation))
                                        .is_err()
                                    {
                                        break;
                                    }
                                    // Readability is not admission. TOML parsing, asset
                                    // preparation, and versioned-service synchronization
                                    // still run asynchronously; only their exact-baseline
                                    // success may clear the native warning. Reset this
                                    // worker latch silently so a later host error begins
                                    // a fresh failure epoch.
                                    config_failure.clear_silently();
                                    config_needs_retry = false;
                                    last_config_stamp = Some(stamp);
                                }
                                Err(error) => {
                                    config_needs_retry = true;
                                    // Retain the last admitted metadata stamp. A
                                    // same-stamp recovery must retry the exact read
                                    // rather than treating metadata alone as proof.
                                    if !post_status(
                                        &proxy,
                                        &mut config_failure,
                                        WatchTarget::Config,
                                        Some(config_failure_kind(&error)),
                                    ) {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if let Some(directory) = theme_dir.as_deref() {
                match theme_directory_stamp(directory) {
                    Err(kind) => {
                        if !post_status(&proxy, &mut theme_failure, WatchTarget::Themes, Some(kind))
                        {
                            break;
                        }
                    }
                    Ok(stamp) => match prepare_theme_edge_with(
                        directory,
                        last_theme_stamp.as_ref(),
                        stamp,
                        || {
                            crate::app_config::ThemeCatalog::try_discover_in(directory)
                                .map(Arc::new)
                                .map_err(theme_discovery_failure_kind)
                        },
                    ) {
                        None => {
                            // A complete metadata scan also proves recovery from
                            // a transient stamp failure when the generation did
                            // not otherwise change. No theme bytes are reopened.
                            if !post_status(&proxy, &mut theme_failure, WatchTarget::Themes, None) {
                                break;
                            }
                        }
                        Some(Err(kind)) => {
                            if !post_status(
                                &proxy,
                                &mut theme_failure,
                                WatchTarget::Themes,
                                Some(kind),
                            ) {
                                break;
                            }
                        }
                        Some(Ok((stable_stamp, discovered))) => {
                            if *discovered != *themes {
                                if proxy
                                    .send_event(Wake::ThemeCatalogChanged(Arc::clone(&discovered)))
                                    .is_err()
                                {
                                    break;
                                }
                                themes = discovered;
                            }
                            // The catalogue and acknowledgement were derived
                            // from one coherent sample. A racing mutation stays
                            // unacknowledged and is retried next poll.
                            last_theme_stamp = Some(stable_stamp);
                            if !post_status(&proxy, &mut theme_failure, WatchTarget::Themes, None) {
                                break;
                            }
                        }
                    },
                }
            }
        }
    });
}

fn post_status(
    proxy: &EventLoopProxy<Wake>,
    latch: &mut FailureLatch,
    target: WatchTarget,
    failure: Option<WatchFailureKind>,
) -> bool {
    latch
        .transition(target, failure)
        .is_none_or(|event| proxy.send_event(Wake::ConfigWatchStatus(event)).is_ok())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfigPathStamp(u64);

/// Run the exact content observation only for a metadata edge or a prior
/// observation failure. Keeping this seam tiny makes the healthy-idle no-read
/// property deterministic in tests without relying on platform I/O counters.
fn prepare_config_edge_with<T>(
    previous: Option<&ConfigPathStamp>,
    current: ConfigPathStamp,
    retry: bool,
    observe: impl FnOnce() -> T,
) -> Option<(ConfigPathStamp, T)> {
    if !retry && previous == Some(&current) {
        return None;
    }
    Some((current, observe()))
}

fn initial_config_observation_changed(
    startup: Option<&crate::native_document_host::AtomicFileBaseline>,
    observed: &crate::native_config_service::ConfigDiskObservation,
) -> bool {
    startup != Some(&observed.baseline)
}

/// Cheap identity/metadata stamp for the config's logical path. No config byte
/// is opened or read here. The logical entry and followed target are both
/// included, so an atomic replace or symlink retarget is an edge. Unix ctime
/// and file identity additionally catch a same-length rewrite whose mtime was
/// restored; Windows creation/write identity is included where exposed.
fn config_path_stamp(path: &std::path::Path) -> Result<ConfigPathStamp, WatchFailureKind> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "aterm-config-metadata-v1".hash(&mut hasher);
    path.hash(&mut hasher);

    let logical = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            "config-path-absent".hash(&mut hasher);
            return Ok(ConfigPathStamp(hasher.finish()));
        }
        Err(_) => return Err(WatchFailureKind::ConfigUnreadable),
    };
    "logical-entry".hash(&mut hasher);
    hash_file_metadata(&logical, &mut hasher);
    if logical.file_type().is_symlink() {
        let target = std::fs::read_link(path).map_err(|_| WatchFailureKind::ConfigUnreadable)?;
        target.hash(&mut hasher);
    }

    match std::fs::metadata(path) {
        Ok(metadata) => {
            "followed-target".hash(&mut hasher);
            hash_file_metadata(&metadata, &mut hasher);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A dangling logical binding is still a stable candidate edge. The
            // exact capability reader decides whether it is an absent config or
            // an unsafe binding before anything is admitted.
            "followed-target-absent".hash(&mut hasher);
        }
        Err(_) => return Err(WatchFailureKind::ConfigUnreadable),
    }
    Ok(ConfigPathStamp(hasher.finish()))
}

/// One safe, bounded generation observation of the config's logical path.
///
/// This deliberately reuses the exact config/Manual file-capability reader:
/// its baseline includes a content fingerprint, regular-file identity, metadata
/// token, and the complete admitted symlink binding. The Manual analysis limit
/// bounds every candidate-edge read; healthy idle polls stop at
/// [`config_path_stamp`]. An oversized file receives the same stable fail-closed
/// result instead of being streamed without a cap. Keeping the typed error
/// variant makes readable↔unreadable transitions observable and lets
/// Settings/Manual explain the rejection without exposing host paths.
#[cfg(test)]
fn config_file_stamp(
    path: &std::path::Path,
) -> Result<
    crate::native_document_host::AtomicFileBaseline,
    crate::native_document_host::DocumentHostError,
> {
    config_file_observation(path).map(|observation| observation.baseline)
}

fn config_file_observation(
    path: &std::path::Path,
) -> Result<
    crate::native_config_service::ConfigDiskObservation,
    crate::native_document_host::DocumentHostError,
> {
    let contents = crate::native_document_host::read_config_atomic_file(
        path,
        crate::native_config_service::MAX_CONFIG_FILE_BYTES,
        true,
    )?;
    let bytes = contents
        .bytes
        .strip_prefix(&[0xef, 0xbb, 0xbf])
        .unwrap_or(&contents.bytes);
    let text = std::str::from_utf8(bytes)
        .map_err(|_| crate::native_document_host::DocumentHostError::InvalidUtf8)?
        .to_owned();
    Ok(crate::native_config_service::ConfigDiskObservation {
        text,
        baseline: contents.baseline,
    })
}

fn config_failure_kind(error: &crate::native_document_host::DocumentHostError) -> WatchFailureKind {
    use crate::native_document_host::DocumentHostError;
    match error {
        DocumentHostError::InvalidUtf8 => WatchFailureKind::ConfigInvalidUtf8,
        DocumentHostError::TooLarge { .. } => WatchFailureKind::ConfigTooLarge,
        DocumentHostError::NotAFile => WatchFailureKind::ConfigNotRegular,
        DocumentHostError::ChangedWhileReading => WatchFailureKind::ConfigChangedWhileReading,
        DocumentHostError::Io { .. } => WatchFailureKind::ConfigUnreadable,
        DocumentHostError::UnsupportedScheme
        | DocumentHostError::RemoteAuthority
        | DocumentHostError::MalformedUri
        | DocumentHostError::InvalidEncoding
        | DocumentHostError::NotAbsolute
        | DocumentHostError::UnknownGrant
        | DocumentHostError::ReadOnlyGrant
        | DocumentHostError::SymlinkComponent { .. }
        | DocumentHostError::TargetRetargeted => WatchFailureKind::ConfigUnsafeBinding,
    }
}

/// Produce a theme catalogue only when stamps bracketing discovery agree.
/// A racing writer is retried a bounded number of times, then left
/// unacknowledged for the next poll.
fn prepare_theme_edge_with(
    path: &std::path::Path,
    previous: Option<&ThemeDirectoryStamp>,
    current: ThemeDirectoryStamp,
    discover: impl FnMut() -> Result<Arc<crate::app_config::ThemeCatalog>, WatchFailureKind>,
) -> Option<Result<(ThemeDirectoryStamp, Arc<crate::app_config::ThemeCatalog>), WatchFailureKind>> {
    if previous == Some(&current) {
        return None;
    }
    Some(coherent_theme_sample_with(path, current, discover))
}

fn coherent_theme_sample_with(
    path: &std::path::Path,
    mut before: ThemeDirectoryStamp,
    mut discover: impl FnMut() -> Result<Arc<crate::app_config::ThemeCatalog>, WatchFailureKind>,
) -> Result<(ThemeDirectoryStamp, Arc<crate::app_config::ThemeCatalog>), WatchFailureKind> {
    const ATTEMPTS: usize = 3;
    for _ in 0..ATTEMPTS {
        let catalog = discover()?;
        let after = theme_directory_stamp(path)?;
        if before == after {
            return Ok((after, catalog));
        }
        before = after;
    }
    Err(WatchFailureKind::ThemeChangedWhileScanning)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ThemeDirectoryStamp {
    fingerprint: u64,
    inspected_entries: usize,
    candidate_metadata: usize,
    truncated: bool,
}

/// Cheap bounded identity/metadata stamp for the theme directory. No theme file
/// is opened and no content byte is read here. On Unix, `dev + ino + ctime(ns)`
/// detects atomic replacement and same-length/same-mtime in-place rewrites; the
/// portable fallback retains length/mtime and creation metadata where exposed.
/// The first content read happens only in `try_discover_in` after this stamp has
/// selected a real candidate edge.
fn theme_directory_stamp(path: &std::path::Path) -> Result<ThemeDirectoryStamp, WatchFailureKind> {
    const MAX_CANDIDATES: usize = 512;
    let directory_metadata = match std::fs::metadata(path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(WatchFailureKind::ThemeDirectoryUnreadable),
    };
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(WatchFailureKind::ThemeDirectoryUnreadable),
    };

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "aterm-theme-metadata-v2".hash(&mut hasher);
    if let Some(metadata) = directory_metadata.as_ref() {
        hash_file_metadata(metadata, &mut hasher);
    } else {
        "theme-directory-absent".hash(&mut hasher);
    }
    let mut candidates = std::collections::BTreeSet::new();
    let mut count = 0usize;
    let mut inspected_entries = 0usize;
    let mut truncated = false;
    if let Some(entries) = entries {
        for (index, result) in entries
            .take(crate::app_config::MAX_USER_THEME_DIRECTORY_ENTRIES + 1)
            .enumerate()
        {
            inspected_entries = inspected_entries.saturating_add(1);
            if index == crate::app_config::MAX_USER_THEME_DIRECTORY_ENTRIES {
                truncated = true;
                break;
            }
            let entry = result.map_err(|_| WatchFailureKind::ThemeEntryUnreadable)?;
            let file_type = entry.file_type().map_err(theme_entry_failure_kind)?;
            if !file_type.is_file()
                || entry.path().extension() != Some(std::ffi::OsStr::new("conf"))
            {
                continue;
            }
            count = count.saturating_add(1);
            candidates.insert(entry.path());
            if candidates.len() > MAX_CANDIDATES {
                candidates.pop_last();
            }
        }
    }
    count.hash(&mut hasher);
    inspected_entries.hash(&mut hasher);
    truncated.hash(&mut hasher);
    let candidate_metadata = candidates.len();
    for candidate in candidates {
        candidate.hash(&mut hasher);
        let metadata = std::fs::symlink_metadata(&candidate).map_err(theme_entry_failure_kind)?;
        hash_file_metadata(&metadata, &mut hasher);
    }
    Ok(ThemeDirectoryStamp {
        fingerprint: hasher.finish(),
        inspected_entries,
        candidate_metadata,
        truncated,
    })
}

fn theme_entry_failure_kind(error: std::io::Error) -> WatchFailureKind {
    if error.kind() == std::io::ErrorKind::NotFound {
        WatchFailureKind::ThemeChangedWhileScanning
    } else {
        WatchFailureKind::ThemeEntryUnreadable
    }
}

fn theme_discovery_failure_kind(
    error: crate::app_config::ThemeCatalogWatchError,
) -> WatchFailureKind {
    match error {
        crate::app_config::ThemeCatalogWatchError::DirectoryUnreadable => {
            WatchFailureKind::ThemeDirectoryUnreadable
        }
        crate::app_config::ThemeCatalogWatchError::EntryUnreadable => {
            WatchFailureKind::ThemeEntryUnreadable
        }
        crate::app_config::ThemeCatalogWatchError::FileUnavailable => {
            WatchFailureKind::ThemeFileUnavailable
        }
    }
}

fn hash_file_metadata(metadata: &std::fs::Metadata, hasher: &mut impl Hasher) {
    metadata.file_type().is_file().hash(hasher);
    metadata.file_type().is_dir().hash(hasher);
    metadata.file_type().is_symlink().hash(hasher);
    metadata.len().hash(hasher);
    metadata.modified().ok().hash(hasher);
    metadata.permissions().readonly().hash(hasher);
    hash_platform_file_identity(metadata, hasher);
}

#[cfg(unix)]
fn hash_platform_file_identity(metadata: &std::fs::Metadata, hasher: &mut impl Hasher) {
    use std::os::unix::fs::MetadataExt;

    metadata.dev().hash(hasher);
    metadata.ino().hash(hasher);
    metadata.mode().hash(hasher);
    metadata.ctime().hash(hasher);
    metadata.ctime_nsec().hash(hasher);
}

#[cfg(windows)]
fn hash_platform_file_identity(metadata: &std::fs::Metadata, hasher: &mut impl Hasher) {
    use std::os::windows::fs::MetadataExt;

    metadata.creation_time().hash(hasher);
    metadata.last_write_time().hash(hasher);
    metadata.file_attributes().hash(hasher);
}

#[cfg(not(any(unix, windows)))]
fn hash_platform_file_identity(_metadata: &std::fs::Metadata, _hasher: &mut impl Hasher) {}

impl crate::App {
    pub(crate) fn note_config_watch_candidate(
        &mut self,
        baseline: crate::native_document_host::AtomicFileBaseline,
    ) {
        self.native_config_external_sequence = self
            .native_config_external_sequence
            .saturating_add(1)
            .max(1);
        self.native_runtime.note_config_watch_candidate(baseline);
    }

    pub(crate) fn config_watch_admission_pending(&self) -> bool {
        self.native_runtime.has_config_watch_candidate()
    }

    pub(crate) fn reject_config_watch_admission_for(
        &mut self,
        baseline: &crate::native_document_host::AtomicFileBaseline,
        kind: WatchFailureKind,
    ) {
        if self
            .native_runtime
            .reject_config_watch_candidate(baseline, kind)
        {
            self.request_redraw_all_windows();
        }
    }

    pub(crate) fn acknowledge_config_watch_admission(
        &mut self,
        baseline: &crate::native_document_host::AtomicFileBaseline,
    ) {
        if self
            .native_runtime
            .acknowledge_config_watch_candidate(baseline)
        {
            self.request_redraw_all_windows();
        }
    }

    /// Reduce one typed watcher edge into process-global native presentation.
    /// Runtime state owns the persistent projection so views opened after a
    /// failure inherit it; ordinary Settings/Editor feedback remains separate.
    pub(crate) fn apply_config_watch_status(&mut self, event: WatchStatusEvent) {
        if self.native_runtime.apply_config_watch_status(event) {
            self.request_redraw_all_windows();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FailureLatch, WatchFailure, WatchFailureKind, WatchStatusEvent, WatchStatusState,
        WatchTarget, coherent_theme_sample_with, config_failure_kind, config_file_observation,
        config_file_stamp, config_path_stamp, initial_config_observation_changed,
        prepare_config_edge_with, prepare_theme_edge_with, theme_directory_stamp,
    };

    #[test]
    fn idle_config_poll_reads_no_content_until_an_identity_edge_or_retry() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-watch-idle-cost-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        std::fs::write(&path, "theme = \"Default\"\n").unwrap();
        let stamp = config_path_stamp(&path).unwrap();
        let mut content_loads = 0;

        let idle = prepare_config_edge_with(Some(&stamp), stamp.clone(), false, || {
            content_loads += 1;
        });
        assert!(idle.is_none());
        assert_eq!(content_loads, 0, "a healthy idle poll must not open TOML");

        let retry = prepare_config_edge_with(Some(&stamp), stamp.clone(), true, || {
            content_loads += 1;
        });
        assert!(
            retry.is_some(),
            "a failed exact observation must be retried"
        );
        assert_eq!(content_loads, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn config_metadata_edge_tracks_same_length_rewrite_with_restored_mtime() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-watch-metadata-edge-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        std::fs::write(&path, b"theme = \"Nord\"\n").unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        let modified = metadata.modified().unwrap();
        let accessed = metadata.accessed().unwrap();
        let before = config_path_stamp(&path).unwrap();

        std::fs::write(&path, b"theme = \"Rose\"\n").unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(modified)
                .set_accessed(accessed),
        )
        .unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
        assert_ne!(config_path_stamp(&path).unwrap(), before);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_handoff_publishes_only_a_generation_newer_than_the_loaded_baseline() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-watch-startup-handoff-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        std::fs::write(&path, "font_px = 12\n").unwrap();
        let startup = config_file_observation(&path).unwrap();
        assert!(!initial_config_observation_changed(
            Some(&startup.baseline),
            &startup
        ));

        std::fs::write(&path, "font_px = 14\n").unwrap();
        let edited = config_file_observation(&path).unwrap();
        assert!(initial_config_observation_changed(
            Some(&startup.baseline),
            &edited
        ));
        assert!(
            initial_config_observation_changed(None, &edited),
            "a startup parse/read fallback has no authority to suppress a readable generation"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn config_stamp_tracks_same_length_content_when_mtime_is_restored() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-watch-preserved-mtime-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        std::fs::write(&path, b"theme = \"Nord\"\n").unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        let modified = metadata.modified().unwrap();
        let accessed = metadata.accessed().unwrap();
        let before = config_file_stamp(&path).unwrap();

        std::fs::write(&path, b"theme = \"Rose\"\n").unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(modified)
                .set_accessed(accessed),
        )
        .unwrap();
        let after_metadata = std::fs::metadata(&path).unwrap();
        assert_eq!(after_metadata.len(), metadata.len());
        assert_eq!(after_metadata.modified().unwrap(), modified);
        assert_ne!(config_file_stamp(&path).unwrap(), before);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn exact_observation_survives_transient_missing_then_restored_b() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-watch-restored-b-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        std::fs::write(&path, "font_px = 12\n").unwrap();
        let a = config_file_observation(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let missing = config_file_observation(&path).unwrap();
        std::fs::write(&path, "font_px = 14\n").unwrap();
        let b = config_file_observation(&path).unwrap();
        assert_ne!(a.baseline, missing.baseline);
        assert_ne!(missing.baseline, b.baseline);
        assert_eq!(b.text, "font_px = 14\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn config_stamp_uses_the_shared_runtime_and_manual_admission_cap() {
        let dir =
            std::env::temp_dir().join(format!("aterm-config-watch-limit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(crate::native_config_service::MAX_CONFIG_FILE_BYTES as u64 + 1)
            .unwrap();
        assert_eq!(
            config_file_stamp(&path),
            Err(crate::native_document_host::DocumentHostError::TooLarge {
                limit: crate::native_config_service::MAX_CONFIG_FILE_BYTES,
            })
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn config_stamp_tracks_symlink_retarget_with_identical_content() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "aterm-config-watch-retarget-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.toml");
        let second = dir.join("second.toml");
        let logical = dir.join("aterm.toml");
        std::fs::write(&first, b"theme = \"Nord\"\n").unwrap();
        std::fs::write(&second, b"theme = \"Nord\"\n").unwrap();
        symlink(&first, &logical).unwrap();
        let before = config_file_stamp(&logical).unwrap();

        std::fs::remove_file(&logical).unwrap();
        symlink(&second, &logical).unwrap();
        let after = config_file_stamp(&logical).unwrap();
        assert_eq!(before.observed.content, after.observed.content);
        assert_ne!(after, before);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn theme_stamp_tracks_regular_conf_content_generation_and_membership() {
        let dir = std::env::temp_dir().join(format!("aterm-theme-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let empty = theme_directory_stamp(&dir).unwrap();
        assert_eq!(empty.candidate_metadata, 0);
        let theme = dir.join("Work.conf");
        std::fs::write(&theme, "foreground = #fff\n").unwrap();
        let created = theme_directory_stamp(&dir).unwrap();
        assert_ne!(created, empty);
        assert_eq!(created.candidate_metadata, 1);
        std::fs::write(&theme, "foreground = #ffffff\nbackground = #000000\n").unwrap();
        let edited = theme_directory_stamp(&dir).unwrap();
        assert_ne!(edited, created);
        std::fs::remove_file(theme).unwrap();
        let deleted = theme_directory_stamp(&dir).unwrap();
        assert_ne!(deleted, edited);
        assert_eq!(deleted.candidate_metadata, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn theme_stamp_tracks_same_length_content_when_mtime_is_restored() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-theme-watch-preserved-mtime-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let theme = dir.join("Work.conf");
        std::fs::write(&theme, b"foreground = #112233\n").unwrap();
        let metadata = std::fs::metadata(&theme).unwrap();
        let modified = metadata.modified().unwrap();
        let accessed = metadata.accessed().unwrap();
        let before = theme_directory_stamp(&dir).unwrap();

        std::fs::write(&theme, b"foreground = #445566\n").unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&theme)
            .unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(modified)
                .set_accessed(accessed),
        )
        .unwrap();
        let after_metadata = std::fs::metadata(&theme).unwrap();
        assert_eq!(after_metadata.len(), metadata.len());
        assert_eq!(after_metadata.modified().unwrap(), modified);
        assert_ne!(theme_directory_stamp(&dir).unwrap(), before);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn theme_sample_retries_mutation_between_stamp_and_discovery() {
        let dir =
            std::env::temp_dir().join(format!("aterm-theme-watch-coherent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let theme = dir.join("Work.conf");
        std::fs::write(&theme, "foreground = #111111\n").unwrap();
        let mut calls = 0;
        let initial = theme_directory_stamp(&dir).unwrap();
        let (_, catalog) = coherent_theme_sample_with(&dir, initial, || {
            calls += 1;
            let catalog = std::sync::Arc::new(crate::app_config::ThemeCatalog::discover_in(&dir));
            if calls == 1 {
                std::fs::write(&theme, "foreground = #222222\n").unwrap();
            }
            Ok(catalog)
        })
        .expect("second sample is stable");
        assert!(calls >= 2, "the split generation must be retried");
        let expected = crate::app_config::ThemeCatalog::discover_in(&dir);
        assert_eq!(*catalog, expected);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn theme_stamp_bounds_total_entries_and_detects_return_under_cap() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-theme-watch-entry-cap-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for index in 0..=crate::app_config::MAX_USER_THEME_DIRECTORY_ENTRIES {
            std::fs::write(dir.join(format!("noise-{index:04}")), []).unwrap();
        }
        let over = theme_directory_stamp(&dir).unwrap();
        assert!(over.truncated);
        assert_eq!(
            over.inspected_entries,
            crate::app_config::MAX_USER_THEME_DIRECTORY_ENTRIES + 1
        );
        std::fs::remove_file(dir.join("noise-0000")).unwrap();
        let under = theme_directory_stamp(&dir).unwrap();
        assert_ne!(over, under, "returning under the entry cap is observable");
        assert!(!under.truncated);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn idle_theme_poll_reads_no_theme_contents_and_bounds_metadata() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-theme-watch-idle-cost-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for index in 0..520 {
            std::fs::write(
                dir.join(format!("Theme-{index:04}.conf")),
                "foreground = #112233\n",
            )
            .unwrap();
        }
        let stamp = theme_directory_stamp(&dir).unwrap();
        assert_eq!(stamp.inspected_entries, 520);
        assert_eq!(stamp.candidate_metadata, 512);

        let mut content_loads = 0;
        let result = prepare_theme_edge_with(&dir, Some(&stamp), stamp.clone(), || {
            content_loads += 1;
            Ok(crate::app_config::ThemeCatalog::empty())
        });
        assert!(result.is_none());
        assert_eq!(content_loads, 0, "an idle poll must not open theme files");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn theme_directory_errors_are_not_empty_catalog_generations() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-theme-watch-read-error-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let not_a_directory = dir.join("themes.conf");
        std::fs::write(&not_a_directory, "foreground = #112233\n").unwrap();
        assert_eq!(
            theme_directory_stamp(&not_a_directory),
            Err(WatchFailureKind::ThemeDirectoryUnreadable)
        );
        assert!(
            crate::app_config::ThemeCatalog::try_discover_in(&not_a_directory).is_err(),
            "a read_dir failure must not manufacture an empty catalog"
        );

        let missing = dir.join("deleted");
        let stamp = theme_directory_stamp(&missing).expect("NotFound is a valid empty generation");
        assert_eq!(stamp.candidate_metadata, 0);
        let empty = crate::app_config::ThemeCatalog::try_discover_in(&missing)
            .expect("NotFound maps to an empty catalog");
        assert_eq!(empty, *crate::app_config::ThemeCatalog::empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failure_edges_are_deduplicated_and_recovery_clears_only_its_target() {
        let mut latch = FailureLatch::default();
        let failure = WatchFailure {
            target: WatchTarget::Config,
            kind: WatchFailureKind::ConfigInvalidUtf8,
        };
        assert_eq!(
            latch.transition(
                WatchTarget::Config,
                Some(WatchFailureKind::ConfigInvalidUtf8)
            ),
            Some(WatchStatusEvent::Failed(failure))
        );
        assert_eq!(
            latch.transition(
                WatchTarget::Config,
                Some(WatchFailureKind::ConfigInvalidUtf8)
            ),
            None,
            "a stable failure posts no repeated wake"
        );
        assert_eq!(
            latch.transition(WatchTarget::Config, None),
            Some(WatchStatusEvent::Recovered(WatchTarget::Config))
        );
        assert_eq!(latch.transition(WatchTarget::Config, None), None);

        let mut state = WatchStatusState::default();
        assert!(state.reduce(WatchStatusEvent::Failed(failure)));
        assert!(state.reduce(WatchStatusEvent::Failed(WatchFailure {
            target: WatchTarget::Themes,
            kind: WatchFailureKind::ThemeDirectoryUnreadable,
        })));
        assert!(state.reduce(WatchStatusEvent::Recovered(WatchTarget::Config)));
        let message = state.message().expect("theme failure remains visible");
        assert!(!message.contains("aterm.toml"));
        assert!(message.contains("Custom themes"));
        assert!(state.reduce(WatchStatusEvent::Recovered(WatchTarget::Themes)));
        assert_eq!(state.message(), None);
    }

    #[test]
    fn watcher_failure_latch_conforms_to_the_derived_protocol() {
        let model = aterm_spec::derive::watcher_failure_recovery_model();
        let mut modeled = model.init_state();
        let mut latch = FailureLatch::default();
        let mut failure_wakes = 0_i64;
        let mut recovery_wakes = 0_i64;

        let failed = latch.transition(
            WatchTarget::Themes,
            Some(WatchFailureKind::ThemeDirectoryUnreadable),
        );
        failure_wakes += i64::from(matches!(failed, Some(WatchStatusEvent::Failed(_))));
        modeled = model.successors("ObserveFailure", &modeled)[0].clone();
        assert_eq!(i64::from(latch.0.is_some()), modeled["failed"]);
        assert_eq!(failure_wakes, modeled["failure_wakes"]);

        let repeated = latch.transition(
            WatchTarget::Themes,
            Some(WatchFailureKind::ThemeDirectoryUnreadable),
        );
        failure_wakes += i64::from(matches!(repeated, Some(WatchStatusEvent::Failed(_))));
        modeled = model.successors("RepeatFailure", &modeled)[0].clone();
        assert_eq!(repeated, None);
        assert_eq!(failure_wakes, modeled["failure_wakes"]);
        assert!(model.check_invariant("FailureWakeDeduped", &modeled));

        let recovered = latch.transition(WatchTarget::Themes, None);
        recovery_wakes += i64::from(matches!(recovered, Some(WatchStatusEvent::Recovered(_))));
        modeled = model.successors("Recover", &modeled)[0].clone();
        assert_eq!(i64::from(latch.0.is_some()), modeled["failed"]);
        assert_eq!(recovery_wakes, modeled["recovery_wakes"]);
        assert!(model.check_invariant("FailureStatusExact", &modeled));

        let mut duplicate = modeled;
        duplicate.insert("failure_wakes", duplicate["failure_wakes"] + 1);
        assert!(
            !model.check_invariant("FailureWakeDeduped", &duplicate),
            "negative control: a duplicate wake must violate the model"
        );
    }

    #[test]
    fn invalid_utf8_is_a_typed_config_rejection() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-watch-invalid-utf8-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        let error = config_file_observation(&path).unwrap_err();
        assert_eq!(
            config_failure_kind(&error),
            WatchFailureKind::ConfigInvalidUtf8
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_the_latest_admitted_config_candidate_can_clear_failure_status() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-watch-admission-ticket-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        std::fs::write(&path, "font_px = 12\n").unwrap();
        let first = config_file_observation(&path).unwrap().baseline;
        std::fs::write(&path, "font_px = 14\n").unwrap();
        let second = config_file_observation(&path).unwrap().baseline;
        assert_ne!(first, second);

        let mut state = WatchStatusState::default();
        let model = aterm_spec::derive::watcher_failure_recovery_model();
        let mut modeled = model.successors("ObserveFailure", &model.init_state())[0].clone();
        assert!(state.reduce(WatchStatusEvent::Failed(WatchFailure {
            target: WatchTarget::Config,
            kind: WatchFailureKind::ConfigInvalidToml,
        })));
        state.note_config_candidate(first.clone());
        modeled = model.successors("ObserveCandidateOne", &modeled)[0].clone();
        state.note_config_candidate(second.clone());
        modeled = model.successors("ObserveCandidateTwo", &modeled)[0].clone();
        assert!(
            !state.reject_config_candidate(&first, WatchFailureKind::ConfigPreparationFailed),
            "a stale rejection cannot invalidate the newer recovery ticket"
        );
        assert!(
            !state.acknowledge_config_candidate(&first),
            "an older async completion cannot clear a newer candidate"
        );
        assert!(state.message().is_some());
        modeled = model.successors("AdmitCandidateOne", &modeled)[0].clone();
        assert_eq!(modeled["failed"], 1);
        assert!(model.check_invariant("ConfigRecoveryAdmitsLatest", &modeled));
        assert_eq!(state.config_candidate.as_ref(), Some(&second));
        assert!(state.acknowledge_config_candidate(&second));
        modeled = model.successors("AdmitCandidateTwo", &modeled)[0].clone();
        assert_eq!(modeled["failed"], 0);
        assert_eq!(state.message(), None);

        state.note_config_candidate(second.clone());
        assert!(state.reduce(WatchStatusEvent::Failed(WatchFailure {
            target: WatchTarget::Config,
            kind: WatchFailureKind::ConfigUnreadable,
        })));
        assert!(
            !state.acknowledge_config_candidate(&second),
            "a later read failure invalidates the pending recovery ticket"
        );
        assert!(state.message().is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}
