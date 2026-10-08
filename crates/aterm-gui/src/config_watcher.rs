// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live config hot-reload watcher.
//!
//! A single background thread observes the user config file
//! (`config_path()` — `$XDG_CONFIG_HOME/aterm/aterm.toml`, else
//! `~/.config/aterm/aterm.toml`) every 500 ms and, when its
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
//! mode denies reads/writes under `~/.config/aterm`). A bounded
//! observation loop adds ZERO new dependencies, no inotify/FSEvents/kqueue file
//! descriptors, and no surprising behavior under a sandbox profile. Config
//! failures are posted as typed, de-duplicated status edges while the previous
//! live generation remains active. Recovery posts a matching clear edge. Theme
//! polling backs off to 2 s while stable and is metadata-only: bounded path/file identity, mtime, and
//! ctime (where the platform exposes it) select a candidate edge; theme bytes are
//! read only while preparing that changed generation. A timestamp-preserving
//! rewrite or atomic replacement therefore cannot leave the process stale on
//! macOS/Linux without continuously hashing tens of MiB of theme data.
//!
//! THE PATH FEEDS ride the config's own edge. The files `aterm.toml` names by
//! path whose content is applied state — the Sparkle Words lexicon, Toy Packs
//! and Trail Packs ([`crate::app_config::Config::path_feed_paths`]) — are
//! stamped beside it, metadata only, on the same 500 ms poll; a change to one
//! of them re-posts the current config observation, and the UI's existing
//! reload admission re-reads the feeds and compares their content
//! fingerprints (`refresh_path_feeds`). So editing only the lexicon hot-reloads
//! it, a `touch` that changes no byte stays a no-op, and there is one reload
//! path, not two.
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
use std::time::{Duration, Instant};

use winit::event_loop::EventLoopProxy;

use crate::Wake;

// THE SUPERVISOR POLICY HAS ONE WRITER, PROVED BY THE COMPILER — see the
// sibling `config_watcher_island.rs`, whose `clean { … }` island the Clean
// kernel checks during every Trust-lane build ([`observed_harness`]).
#[cfg(clean_islands)]
#[path = "config_watcher_island.rs"]
mod island;

/// How often the watcher observes the config file. 500 ms is imperceptible for
/// an interactive "save and see it apply" loop. Healthy idle polls read only
/// bounded metadata. Candidate-edge reads share Manual's 512-KiB cap, and the
/// thread is parked in `sleep` between polls.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// A stable theme directory needs a bounded rescan, but not four directory
/// walks per two seconds. Config observation and theme edge follow-ups retain
/// the 500 ms cadence; only unchanged theme generations use this interval.
const THEME_IDLE_POLL_INTERVAL: Duration = Duration::from_secs(2);

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
        let mut messages: Vec<String> = Vec::with_capacity(2);
        if let Some(kind) = self.config {
            let mut message = failure_message(kind).to_string();
            // "Kept unchanged" protects a loaded configuration. When the launch
            // could not read the file there is none: say what is actually
            // running, so the reload refusal cannot read as "your settings are
            // safe" over a window on all-defaults (glass hunt 2026-09-01, #2).
            if let Some(notice) = crate::app_config::launch_load_failure() {
                // An invalid file still had its [harness] lines read at launch
                // (`Config::stand_in`); an unreadable one had nothing read.
                let running = if notice.starts_with("aterm.toml is not a valid configuration") {
                    "the window's settings except [harness] are at their defaults"
                } else {
                    "every setting is at its default"
                };
                message.push_str(&format!(
                    " (The file did not load at launch either: {running} until it does.)"
                ));
            }
            messages.push(message);
        }
        if let Some(kind) = self.themes {
            messages.push(failure_message(kind).to_string());
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
struct FailureLatch {
    kind: Option<WatchFailureKind>,
    /// The window's view of this status is unknown (its wakes were dropped
    /// before a seamless successor's Commit): the next transition posts its
    /// edge whatever it is.
    unknown: bool,
}

impl FailureLatch {
    fn transition(
        &mut self,
        target: WatchTarget,
        failure: Option<WatchFailureKind>,
    ) -> Option<WatchStatusEvent> {
        let unknown = std::mem::take(&mut self.unknown);
        if !unknown && self.kind == failure {
            return None;
        }
        let previous = self.kind;
        self.kind = failure;
        match failure {
            Some(kind) => Some(WatchStatusEvent::Failed(WatchFailure { target, kind })),
            None if previous.is_some() || unknown => Some(WatchStatusEvent::Recovered(target)),
            None => None,
        }
    }

    fn clear_silently(&mut self) {
        self.kind = None;
        self.unknown = false;
    }

    /// Forget what the window was told: its status wakes were dropped before
    /// Commit, so the next outcome — failure or recovery — is posted.
    fn forget(&mut self) {
        self.kind = None;
        self.unknown = true;
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
pub(crate) fn spawn(
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
        // The path feeds the observed config names, and their metadata stamp
        // taken BEFORE the UI reads them: a later edit is always an edge.
        let mut feed_paths = match initial_config.as_ref() {
            Some(Ok(observation)) => feed_paths_of(&observation.text).unwrap_or_default(),
            _ => Vec::new(),
        };
        let mut last_feed_stamp = path_feed_stamp(&feed_paths);
        let mut config_needs_retry = initial_config.as_ref().is_some_and(Result::is_err);
        let mut config_failure = FailureLatch::default();
        // THE ONE WRITER OF THE SUPERVISOR'S POLICY (2026-09-29): the
        // `[harness]` policy the window runs, as last posted
        // ([`observed_harness`], [`post_harness`]). This thread reads the file
        // in disk order and its wakes arrive in send order, so the window runs
        // the `[harness]` of the newest file read — whatever the config lane's
        // workers finish when. `None` until the first post, which is made
        // unconditionally: the launch read and this one may differ.
        let mut last_harness = None;
        // Every post carries the next generation of this one source, so the
        // window's adoption is a join (`harness_policy_cell`).
        let mut harness_stamps = crate::harness_policy_cell::HarnessStampSource::new();
        if let Some(initial) = initial_config.as_ref()
            && let Some(policy) = observed_harness(path.as_deref(), initial)
            && !post_harness(
                send_harness(&proxy),
                &mut last_harness,
                &mut harness_stamps,
                policy,
            )
        {
            return;
        }
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
        let mut theme_poll = ThemePollSchedule::new(Instant::now(), initial_theme_stamp.clone());
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
        // Set at Commit: the next discovered catalogue is posted even if equal.
        let mut theme_repost = false;
        loop {
            std::thread::sleep(POLL_INTERVAL);
            // A seamless successor drops every config wake that arrives
            // before its Commit, so at Commit the next polls re-read and
            // re-post the file and the theme catalogue as they stand (review
            // rounds 3 to 5, 2026-09-29): the `[harness]` policy and both
            // statuses are posted once per change and would otherwise never
            // arrive, a readable edit dropped then would wait for the next
            // save, and a theme change seen then for the next theme edit.
            if REPOST_AFTER_COMMIT.swap(false, std::sync::atomic::Ordering::Relaxed) {
                last_harness = None;
                config_failure.forget();
                config_needs_retry = true;
                theme_failure.forget();
                last_theme_stamp = None;
                theme_repost = true;
            }
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
                        // A path feed moved under an unchanged `aterm.toml`: the
                        // same edge as the config's own, so the current
                        // observation is re-posted and admission re-reads feeds.
                        let feeds_moved = path_feed_stamp(&feed_paths) != last_feed_stamp;
                        let candidate = prepare_config_edge_with(
                            last_config_stamp.as_ref(),
                            stamp,
                            config_needs_retry || feeds_moved,
                            || config_file_observation(path),
                        );
                        if let Some((stamp, observation)) = candidate {
                            // THE POLICY FIRST, for a refused file as for a
                            // readable one: the window runs the `[harness]` of
                            // the newest read, and a readable observation's
                            // admission then reports under the policy it runs.
                            if let Some(policy) = observed_harness(Some(path), &observation)
                                && !post_harness(
                                    send_harness(&proxy),
                                    &mut last_harness,
                                    &mut harness_stamps,
                                    policy,
                                )
                            {
                                break;
                            }
                            match observation {
                                Ok(observation) => {
                                    // Re-derive the feed set from THIS text and stamp it
                                    // before posting, so an edit racing the UI's read is
                                    // the next poll's edge. An unparseable config keeps
                                    // the last known set (admission rejects the text).
                                    if let Some(paths) = feed_paths_of(&observation.text) {
                                        feed_paths = paths;
                                    }
                                    last_feed_stamp = path_feed_stamp(&feed_paths);
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

            if let Some(directory) = theme_dir.as_deref()
                && theme_poll.due(Instant::now())
            {
                let sample = match theme_directory_stamp(directory) {
                    Err(kind) => {
                        if !post_status(&proxy, &mut theme_failure, WatchTarget::Themes, Some(kind))
                        {
                            break;
                        }
                        Err(kind)
                    }
                    Ok(stamp) => {
                        let sampled_stamp = stamp.clone();
                        match prepare_theme_edge_with(
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
                                if !post_status(
                                    &proxy,
                                    &mut theme_failure,
                                    WatchTarget::Themes,
                                    None,
                                ) {
                                    break;
                                }
                                Ok(sampled_stamp)
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
                                Err(kind)
                            }
                            Some(Ok((stable_stamp, discovered))) => {
                                if theme_repost || *discovered != *themes {
                                    if proxy
                                        .send_event(Wake::ThemeCatalogChanged(Arc::clone(
                                            &discovered,
                                        )))
                                        .is_err()
                                    {
                                        break;
                                    }
                                    themes = discovered;
                                    theme_repost = false;
                                }
                                // The catalogue and acknowledgement were derived
                                // from one coherent sample. A racing mutation stays
                                // unacknowledged and is retried next poll.
                                last_theme_stamp = Some(stable_stamp.clone());
                                if !post_status(
                                    &proxy,
                                    &mut theme_failure,
                                    WatchTarget::Themes,
                                    None,
                                ) {
                                    break;
                                }
                                Ok(stable_stamp)
                            }
                        }
                    }
                };
                theme_poll.observed(Instant::now(), sample);
            }
        }
    });
}

/// The largest refused config [`observed_harness`] still reads (8 MiB).
const MAX_REJECTED_HARNESS_READ: u64 = 8 << 20;

/// The `[harness]` policy of one watcher read — the ONE writer of the policy
/// the window runs (2026-09-29; `App::adopt_harness_policy` has no other
/// caller). A readable file's is the one its text writes, exactly what
/// `Config::parse` reads; a file the reload refuses for its ENCODING or SIZE
/// still writes `[harness]` lines, read lossily from a bounded re-read as the
/// launch reads them, so a limit saved beside a stray byte holds at once, as
/// it would at the next launch. `None` for any other refusal, and for a file
/// gone or unreadable by the re-read: nothing written, nothing posted.
///
/// WHY ONE WRITER (review rounds 1 to 5, 2026-09-29): the policy used to be
/// adopted by the config lane when a generation was admitted, and by this
/// thread for a refused file — two writers, two queues. The lane's workers
/// (preparation, the font catalogue, the reconciliation fence) finish in no
/// order this thread controls, so a valid edit still in the lane when the
/// next save was refused landed afterwards and lifted the refused file's kill
/// switch; each patch (an override, its candidate, a post order) left an
/// invariant spread across modules that the next review broke. With this
/// thread the only writer and its wakes in send order, the running policy is
/// the newest read's by construction. The `clean { … }` island in
/// `config_watcher_island.rs` states the protocol and the kernel checks it at
/// every Trust build.
fn observed_harness(
    path: Option<&std::path::Path>,
    observation: &Result<
        crate::native_config_service::ConfigDiskObservation,
        crate::native_document_host::DocumentHostError,
    >,
) -> Option<crate::app_config::HarnessPolicy> {
    use crate::native_document_host::DocumentHostError;
    use std::io::Read as _;
    match observation {
        Ok(observation) => Some(crate::app_config::HarnessPolicy::read(&observation.text)),
        Err(DocumentHostError::InvalidUtf8 | DocumentHostError::TooLarge { .. }) => {
            // The retry re-reads every poll: past a bound, a runaway file is
            // left to the refusal alone (one stat) rather than streamed each
            // time, and the read stays bounded for a file that grows under it.
            let file = std::fs::File::open(path?).ok()?;
            if file.metadata().ok()?.len() > MAX_REJECTED_HARNESS_READ {
                return None;
            }
            let mut bytes = Vec::new();
            file.take(MAX_REJECTED_HARNESS_READ + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            if bytes.len() as u64 > MAX_REJECTED_HARNESS_READ {
                return None;
            }
            Some(crate::app_config::HarnessPolicy::read(
                &String::from_utf8_lossy(&bytes),
            ))
        }
        Err(_) => None,
    }
}

/// Post `policy` as the one the window runs when it differs from the one last
/// posted (`last`), recording it only once `send` queued the wake
/// (`Wake::HarnessPolicyObserved`). `false` when the event loop is gone.
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "HarnessPolicyOneWriter",
        action = "Poll",
        project = "aterm_gui::config_watcher::tests::project_harness_writer"
    )
)]
fn post_harness(
    send: impl FnOnce(crate::harness_policy_cell::StampedHarnessPolicy) -> bool,
    last: &mut Option<crate::app_config::HarnessPolicy>,
    stamps: &mut crate::harness_policy_cell::HarnessStampSource,
    policy: crate::app_config::HarnessPolicy,
) -> bool {
    if last.as_ref() == Some(&policy) {
        return true;
    }
    if !send(stamps.stamp(policy.clone())) {
        return false;
    }
    *last = Some(policy);
    true
}

/// The watcher's post of a policy to the window.
fn send_harness(
    proxy: &EventLoopProxy<Wake>,
) -> impl FnOnce(crate::harness_policy_cell::StampedHarnessPolicy) -> bool + '_ {
    move |policy| {
        proxy
            .send_event(Wake::HarnessPolicyObserved(policy))
            .is_ok()
    }
}

/// Set at a seamless successor's Commit ([`repost_after_commit`]); the
/// watcher's next poll re-posts the config file as it stands.
static REPOST_AFTER_COMMIT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The successor reached Commit: every config wake before it was dropped
/// (`App::drops_wake_before_commit`), so the watcher posts the file again.
#[cfg(unix)]
pub(crate) fn repost_after_commit() {
    REPOST_AFTER_COMMIT.store(true, std::sync::atomic::Ordering::Relaxed);
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

/// The path feeds a config TEXT names (`Config::path_feed_paths`), or `None`
/// when the text does not parse.
fn feed_paths_of(text: &str) -> Option<Vec<std::path::PathBuf>> {
    crate::app_config::Config::parse(text)
        .ok()
        .map(|config| config.path_feed_paths())
}

/// Metadata-only stamp over the path feeds: the logical entry, a symlink's
/// target and the followed file for each, or its absence. No feed byte is
/// opened here — the content fingerprint is admission's job. A warm successor
/// stamps every file its early warm reads with it (`crate::WarmConfigRead`).
pub(crate) fn path_feed_stamp(paths: &[std::path::PathBuf]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "aterm-path-feed-metadata-v1".hash(&mut hasher);
    for path in paths {
        path.hash(&mut hasher);
        match std::fs::symlink_metadata(path) {
            Ok(logical) => {
                hash_file_metadata(&logical, &mut hasher);
                if logical.file_type().is_symlink() {
                    std::fs::read_link(path).ok().hash(&mut hasher);
                }
            }
            Err(_) => "absent".hash(&mut hasher),
        }
        match std::fs::metadata(path) {
            Ok(followed) => hash_file_metadata(&followed, &mut hasher),
            Err(_) => "target-absent".hash(&mut hasher),
        }
    }
    hasher.finish()
}

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
        // An evicted (dataless) config file cannot be read on a thread whose
        // materialization policy is OFF; to the watcher that is "unreadable".
        DocumentHostError::Io { .. } => WatchFailureKind::ConfigUnreadable,
        #[cfg(unix)]
        DocumentHostError::NotDownloaded { .. } => WatchFailureKind::ConfigUnreadable,
        DocumentHostError::UnsupportedScheme
        | DocumentHostError::RemoteAuthority
        | DocumentHostError::MalformedUri
        | DocumentHostError::InvalidEncoding
        | DocumentHostError::NotAbsolute
        | DocumentHostError::UnknownGrant
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

/// Keep the 500 ms startup reconciliation and one fast follow-up after an
/// observed theme change or failure transition. A stable directory then costs
/// one bounded scan every 2 s. An independent theme edit can therefore wait
/// roughly 2 s plus scheduling/work delay; config file latency is unchanged.
struct ThemePollSchedule {
    next_poll: Instant,
    last_sample: Option<Result<ThemeDirectoryStamp, WatchFailureKind>>,
}

impl ThemePollSchedule {
    fn new(now: Instant, initial: Option<Result<ThemeDirectoryStamp, WatchFailureKind>>) -> Self {
        Self {
            next_poll: now + POLL_INTERVAL,
            last_sample: initial,
        }
    }

    fn due(&self, now: Instant) -> bool {
        now >= self.next_poll
    }

    fn observed(&mut self, now: Instant, sample: Result<ThemeDirectoryStamp, WatchFailureKind>) {
        let changed = self.last_sample.as_ref() != Some(&sample);
        self.last_sample = Some(sample);
        self.next_poll = now
            + if changed {
                POLL_INTERVAL
            } else {
                THEME_IDLE_POLL_INTERVAL
            };
    }
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
pub(crate) mod tests {
    use super::post_harness;
    use super::{
        FailureLatch, POLL_INTERVAL, THEME_IDLE_POLL_INTERVAL, ThemePollSchedule, WatchFailure,
        WatchFailureKind, WatchStatusEvent, WatchStatusState, WatchTarget,
        coherent_theme_sample_with, config_failure_kind, config_file_observation,
        config_file_stamp, config_path_stamp, feed_paths_of, initial_config_observation_changed,
        observed_harness, path_feed_stamp, prepare_config_edge_with, prepare_theme_edge_with,
        theme_directory_stamp,
    };

    /// EDITING ONLY THE LEXICON HOT-RELOADS IT
    /// (`docs/sparkle-words-design.md` §non-goals, retired 2026-09-25). The
    /// feeds the config names are stamped beside it; a rewrite of the lexicon
    /// file — same length, mtime restored — under an unchanged `aterm.toml`
    /// moves the feed stamp, which is what re-posts the observation. RED
    /// before: the watcher stamped `aterm.toml` alone, so the edit waited for
    /// the next config save. Control: an idle poll moves nothing.
    #[cfg(unix)]
    #[test]
    fn a_lexicon_edit_under_an_unchanged_config_is_a_reload_edge() {
        let dir =
            std::env::temp_dir().join(format!("aterm-config-watch-lexicon-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("aterm.toml");
        let lexicon = dir.join("lexicon.toml");
        let pack = dir.join("pack.toml");
        std::fs::write(&lexicon, b"[[entry]]\nword = \"purr\"\n").unwrap();
        std::fs::write(
            &config,
            format!(
                "cursor_trail_packs = [{:?}]\n\n[sparkle_words]\nlexicon = {:?}\n",
                pack.display().to_string(),
                lexicon.display().to_string(),
            ),
        )
        .unwrap();
        let observation = config_file_observation(&config).unwrap();
        let feeds = feed_paths_of(&observation.text).expect("the config parses");
        assert_eq!(feeds, vec![lexicon.clone(), pack.clone()]);
        let config_stamp = config_path_stamp(&config).unwrap();
        let before = path_feed_stamp(&feeds);
        assert_eq!(path_feed_stamp(&feeds), before, "control: idle is no edge");

        let metadata = std::fs::metadata(&lexicon).unwrap();
        let modified = metadata.modified().unwrap();
        std::fs::write(&lexicon, b"[[entry]]\nword = \"mews\"\n").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&lexicon)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert_eq!(config_path_stamp(&config).unwrap(), config_stamp);
        let moved = path_feed_stamp(&feeds) != before;
        assert!(moved, "a lexicon rewrite moves the feed stamp");
        let mut observed = 0;
        let edge =
            prepare_config_edge_with(Some(&config_stamp), config_stamp.clone(), moved, || {
                observed += 1;
            });
        assert!(
            edge.is_some() && observed == 1,
            "…and re-observes the config"
        );

        // A feed that appears is an edge too (it was absent above).
        let absent = path_feed_stamp(&feeds);
        std::fs::write(&pack, b"").unwrap();
        assert_ne!(path_feed_stamp(&feeds), absent);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A DISABLED Sparkle table names no feed (it has no consumer), and the
    /// lists are capped exactly as their loaders cap them.
    #[test]
    fn path_feed_paths_follow_their_consumers() {
        let off = crate::app_config::Config::parse(
            "[sparkle_words]\nenabled = false\nlexicon = \"/l.toml\"\ntoy_packs = [\"/t.toml\"]\n",
        )
        .unwrap();
        assert!(off.path_feed_paths().is_empty());
        let on = crate::app_config::Config::parse(
            "[sparkle_words]\nlexicon = \"/l.toml\"\ntoy_packs = [\"/t.toml\"]\n",
        )
        .unwrap();
        assert_eq!(
            on.path_feed_paths(),
            vec![
                std::path::PathBuf::from("/l.toml"),
                std::path::PathBuf::from("/t.toml")
            ]
        );
        let many: Vec<String> = (0..12).map(|i| format!("\"/p{i}.toml\"")).collect();
        let packs = crate::app_config::Config::parse(&format!(
            "cursor_trail_packs = [{}]\n",
            many.join(", ")
        ))
        .unwrap();
        assert_eq!(packs.path_feed_paths().len(), 8, "the loaders' cap");
    }

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

    /// Tier-1 projection for `HarnessPolicyOneWriter`: the file on disk, the
    /// watcher's dedupe (`post_harness`'s `last`), its post in flight, the
    /// policy the window runs (`App::config.harness`), whether the window runs
    /// what it handles yet, and the Commit's pending forget. A policy projects
    /// to 1 when it approves, 0 for the kill switch.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn project_harness_writer(
        model: &aterm_spec::derive::Model,
        disk: i64,
        edits: i64,
        last: Option<&crate::app_config::HarnessPolicy>,
        queued: Option<&crate::app_config::HarnessPolicy>,
        in_force: &crate::app_config::HarnessPolicy,
        committed: bool,
        forget_pending: bool,
    ) -> aterm_spec::interp::State {
        let bit = |p: &crate::app_config::HarnessPolicy| i64::from(p.policy.enabled);
        let mut state = model.init_state();
        state.insert("disk", disk);
        state.insert("edits", edits);
        state.insert("has_posted", i64::from(last.is_some()));
        state.insert("last_posted", last.map_or(0, bit));
        state.insert("queued", i64::from(queued.is_some()));
        state.insert("queued_val", queued.map_or(0, bit));
        state.insert("in_force", bit(in_force));
        state.insert("committed", i64::from(committed));
        state.insert("forget_pending", i64::from(forget_pending));
        state
    }

    /// `HarnessPolicyOneWriter`'s actions with no shipping function the
    /// Tier-1 drives, each waived by name. Bound: `Poll` (`post_harness`),
    /// `Deliver` (`App::adopt_harness_policy`), `AdmitOn`/`AdmitOff`
    /// (`App::apply_prepared_config_generation`).
    #[aterm_spec::spec_unmodeled(
        machine = "HarnessPolicyOneWriter",
        action = "WriteOn",
        reason = "The environment: a person saves aterm.toml with approvals on. The Tier-1 \
                  writes the real file and the real watcher read (observed_harness) sees it."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "HarnessPolicyOneWriter",
        action = "WriteOff",
        reason = "The environment: a person saves the kill switch, refused for a stray \
                  non-UTF-8 byte. The Tier-1 writes the real file and the real watcher read \
                  (observed_harness) sees it."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "HarnessPolicyOneWriter",
        action = "Commit",
        reason = "A seamless successor's Commit (Wake::ActivateCommittedHandoff), bound by the \
                  native-update handoff machines. Its one effect here, repost_after_commit, \
                  raises the process-wide REPOST_AFTER_COMMIT that a live watcher thread in \
                  the same test binary could consume, so the Tier-1 stands in for it."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "HarnessPolicyOneWriter",
        action = "Forget",
        reason = "An inline block of the watcher thread's loop (REPOST_AFTER_COMMIT's swap, \
                  then last_harness = None), which the Tier-1 transcribes: the loop polls \
                  forever and posts through a real event-loop proxy."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "HarnessPolicyOneWriter",
        action = "SecondWriterOff",
        reason = "The Buggy=1 mutant: an admission adopting its own kill switch, the design \
                  this replaced; no shipping code, and the Tier-1's negative control sees the \
                  step refused."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "HarnessPolicyOneWriter",
        action = "SecondWriterOn",
        reason = "The Buggy=1 mutant: an admission adopting its own approvals, the design this \
                  replaced; no shipping code, and the Tier-1's negative control sees the step \
                  refused."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "HarnessPolicyOneWriter",
        action = "CommitWithoutForget",
        reason = "The Buggy=1 mutant: a Commit that raises no REPOST_AFTER_COMMIT; no shipping \
                  code (the Commit handler calls repost_after_commit)."
    )]
    #[expect(
        dead_code,
        reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it"
    )]
    fn harness_policy_one_writer_waivers() {}

    /// The real parts of the one-writer protocol, driven step by step.
    struct HarnessWriterDrive {
        dir: std::path::PathBuf,
        path: std::path::PathBuf,
        app: crate::App,
        last: Option<crate::app_config::HarnessPolicy>,
        /// The watcher's one source of stamps.
        stamps: crate::harness_policy_cell::HarnessStampSource,
        /// The watcher's post in flight in the event loop's queue.
        queued: Option<crate::harness_policy_cell::StampedHarnessPolicy>,
        committed: bool,
        forget_pending: bool,
        disk: i64,
        edits: i64,
        model: aterm_spec::derive::Model,
    }

    impl HarnessWriterDrive {
        fn state(&self) -> aterm_spec::interp::State {
            project_harness_writer(
                &self.model,
                self.disk,
                self.edits,
                self.last.as_ref(),
                self.queued.as_ref().map(|stamped| stamped.policy()),
                self.app.harness_policy.get(),
                self.committed,
                self.forget_pending,
            )
        }

        /// Run `act` and validate the real transition against `action` (both
        /// tiers); a transition that moves nothing is a stutter.
        fn step(&mut self, action: &str, act: impl FnOnce(&mut Self)) {
            let before = self.state();
            act(self);
            let after = self.state();
            if before == after {
                return;
            }
            let (conforms, evidence) = aterm_spec::verify::validate_transition_tiered(
                &self.model,
                &[],
                &before,
                &after,
                Some(action),
                action,
            );
            assert!(conforms, "{action}: {before:?} -> {after:?}: {evidence}");
            for invariant in &self.model.invariants {
                assert!(
                    self.model.check_invariant(invariant.name, &after),
                    "{action}: {} fails in {after:?}",
                    invariant.name
                );
            }
        }

        /// The file changes on disk: the kill switch goes in REFUSED (a stray
        /// non-UTF-8 byte), approvals go in readable.
        fn write(&mut self, approve: bool) {
            let action = if approve { "WriteOn" } else { "WriteOff" };
            self.step(action, |d| {
                if approve {
                    std::fs::write(&d.path, "font_px = 14\n[harness]\napprove = \"all\"\n")
                        .unwrap();
                } else {
                    std::fs::write(&d.path, b"# caf\xe9\n[harness]\nenabled = false\n").unwrap();
                }
                d.disk = i64::from(approve);
                d.edits += 1;
            });
        }

        /// One watcher read of the file and its post: the real
        /// `observed_harness` and `post_harness`, whose send queues the wake.
        fn poll(&mut self) {
            if self.queued.is_some() {
                return; // the model holds one post in flight
            }
            self.step("Poll", |d| {
                let read = config_file_observation(&d.path);
                let policy = observed_harness(Some(&d.path), &read).expect("a policy per read");
                let queue = &mut d.queued;
                assert!(post_harness(
                    |stamped| {
                        *queue = Some(stamped);
                        true
                    },
                    &mut d.last,
                    &mut d.stamps,
                    policy,
                ));
            });
        }

        /// The window handles the queued post: the real handler
        /// (`adopt_harness_policy`) once it runs what it handles; dropped
        /// before a seamless successor's Commit.
        fn deliver(&mut self) {
            self.step("Deliver", |d| {
                if let Some(policy) = d.queued.take()
                    && d.committed
                {
                    d.app.adopt_harness_policy(policy);
                }
            });
        }

        /// The config lane admits a generation carrying its OWN parse — the
        /// real `apply_prepared_config_generation`, fed a stale file.
        fn admit(&mut self, approve: bool) {
            let action = if approve { "AdmitOn" } else { "AdmitOff" };
            self.step(action, |d| {
                let text = if approve {
                    "font_px = 16\n[harness]\napprove = \"all\"\n"
                } else {
                    "font_px = 12\n[harness]\nenabled = false\n"
                };
                let staged = d.dir.join("admitted.toml");
                std::fs::write(&staged, text).unwrap();
                let observation =
                    crate::native_config_service::VersionedConfigService::observe_path(
                        &staged, false,
                    )
                    .unwrap();
                let config = crate::app_config::Config::parse(text).unwrap();
                assert_eq!(
                    config.harness.policy.enabled, approve,
                    "control: its own parse"
                );
                let assets = config.resolve_asset_catalog();
                let feeds = config.path_feed_fingerprints();
                let sparkle = config.prepare_sparkle_runtime();
                d.app.apply_prepared_config_generation(
                    crate::native_font_catalog::PreparedConfigGeneration {
                        observation,
                        config,
                        values: std::collections::BTreeMap::new(),
                        disk_faults: std::sync::Arc::default(),
                        assets,
                        path_feed_fps: feeds,
                        sparkle,
                        fonts: None,
                        warnings: Vec::new(),
                    },
                );
            });
        }

        /// A seamless successor's Commit: the window runs what it handles from
        /// here on, and the handler raises `REPOST_AFTER_COMMIT`.
        fn commit(&mut self) {
            self.step("Commit", |d| {
                d.committed = true;
                d.forget_pending = true;
            });
        }

        /// The watcher's next loop consumes the flag and forgets what it posted
        /// (the `REPOST_AFTER_COMMIT` block's `last_harness = None`).
        fn forget(&mut self) {
            self.step("Forget", |d| {
                if std::mem::take(&mut d.forget_pending) {
                    d.last = None;
                }
            });
        }
    }

    /// TIER-1 FOR `HarnessPolicyOneWriter` (2026-09-29). The watcher is the
    /// supervisor policy's one writer: this drives the real read
    /// (`observed_harness`), the real post and dedupe (`post_harness`), the
    /// window's real handler and the real admission path through writes of a
    /// refused kill switch and a readable approval, polls before and after a
    /// seamless Commit, and stale admissions of both policies landing at the
    /// worst moments — validating every transition against the derived model
    /// with the interpreter and `ty`, and its invariant after each. NEGATIVE
    /// CONTROL: the step the design this replaced took (a stale admission
    /// moving the policy) is rejected by the same model.
    #[test]
    fn the_watcher_is_the_policys_one_writer() {
        let dir =
            std::env::temp_dir().join(format!("aterm-harness-one-writer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        std::fs::write(&path, b"# caf\xe9\n[harness]\nenabled = false\n").unwrap();
        let app = crate::App::headless_for_test();
        assert!(
            app.harness_policy.get().policy.enabled,
            "the launch ran approvals on"
        );
        let mut drive = HarnessWriterDrive {
            dir: dir.clone(),
            path,
            app,
            last: None,
            stamps: crate::harness_policy_cell::HarnessStampSource::new(),
            queued: None,
            committed: false,
            forget_pending: false,
            disk: 0,
            edits: 0,
            model: aterm_spec::derive::harness_policy_one_writer_model(),
        };
        assert_eq!(drive.state(), drive.model.init_state(), "the model's start");

        drive.poll(); // before Commit: posted …
        drive.deliver(); // … and dropped
        drive.commit();
        drive.poll(); // deduped: the forget has not run yet
        drive.forget();
        drive.poll(); // the repost …
        drive.admit(true); // … a stale approval lands while it is in flight …
        drive.deliver(); // … and the kill switch runs
        assert!(
            !drive.app.harness_policy.get().policy.enabled,
            "the kill switch runs"
        );
        drive.admit(true); // a stale approval lands late: nothing moves
        assert!(
            !drive.app.harness_policy.get().policy.enabled,
            "it still runs"
        );
        drive.poll(); // deduped
        drive.write(true);
        drive.admit(false); // an admission lands before the watcher reads
        assert!(
            !drive.app.harness_policy.get().policy.enabled,
            "only the watcher's read moves the policy"
        );
        drive.poll();
        drive.deliver();
        assert!(
            drive.app.harness_policy.get().policy.enabled,
            "the approval runs"
        );
        drive.write(false);
        drive.poll();
        drive.write(true); // the file changes while the post is in flight
        drive.admit(true);
        drive.deliver(); // the kill switch read runs …
        assert!(!drive.app.harness_policy.get().policy.enabled);
        drive.poll(); // … until the next read of the approval
        drive.deliver();
        assert!(
            drive.app.harness_policy.get().policy.enabled,
            "the newest read runs"
        );

        // NEGATIVE CONTROL: an admission that moves the policy.
        let before = drive.state();
        let mut mutant = before.clone();
        mutant.insert("in_force", 1 - before["in_force"]);
        let (conforms, evidence) = aterm_spec::verify::validate_transition_tiered(
            &drive.model,
            &[],
            &before,
            &mutant,
            Some(if before["in_force"] == 1 {
                "AdmitOff"
            } else {
                "AdmitOn"
            }),
            "second writer",
        );
        assert!(!conforms, "a second writer conformed: {evidence}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// EVERY WATCHER READ CARRIES ITS `[harness]` POLICY, A REFUSED ONE TOO
    /// (2026-09-29). A readable file's policy is exactly the one
    /// `Config::parse` reads from the same text; a file refused for a stray
    /// non-UTF-8 byte, or past the reload's size limit, still writes its
    /// `[harness]` lines, read lossily as the launch reads them — so a kill
    /// switch saved beside the byte holds at once. NEGATIVE CONTROLS: a
    /// refusal of another kind writes no policy, and neither does a refused
    /// file gone before the re-read — never the defaults.
    #[test]
    fn every_watcher_read_carries_its_harness_policy() {
        use crate::native_document_host::DocumentHostError;
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-watch-observed-harness-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");

        let text = "font_px = 13\n[harness]\napprove = \"safe\"\nenabled = false\n";
        std::fs::write(&path, text).unwrap();
        let readable = config_file_observation(&path);
        let policy = observed_harness(Some(&path), &readable).expect("a readable read");
        assert_eq!(
            policy,
            crate::app_config::Config::parse(text).unwrap().harness,
            "the watcher's policy is the one the config's own parse reads"
        );
        assert!(!policy.policy.enabled, "{policy:?}");

        std::fs::write(&path, b"# caf\xe9\n[harness]\nenabled = false\n").unwrap();
        let refused = config_file_observation(&path);
        assert!(
            matches!(refused, Err(DocumentHostError::InvalidUtf8)),
            "{refused:?}"
        );
        let policy = observed_harness(Some(&path), &refused).expect("a refused read");
        assert!(!policy.policy.enabled, "the kill switch holds: {policy:?}");

        std::fs::write(&path, b"# caf\xe9\n[harness]\napprove = \"all\"\n").unwrap();
        let refused = config_file_observation(&path);
        let lifted = observed_harness(Some(&path), &refused).expect("a refused read");
        assert!(lifted.policy.enabled, "lifting it holds too: {lifted:?}");

        let big = format!(
            "[harness]\nenabled = false\n# {}\n",
            "x".repeat(crate::native_config_service::MAX_CONFIG_FILE_BYTES)
        );
        std::fs::write(&path, big).unwrap();
        let refused = config_file_observation(&path);
        assert!(
            matches!(refused, Err(DocumentHostError::TooLarge { .. })),
            "{refused:?}"
        );
        let policy = observed_harness(Some(&path), &refused).expect("a refused read");
        assert!(!policy.policy.enabled, "{policy:?}");

        // Negative controls.
        std::fs::remove_file(&path).unwrap();
        assert!(
            observed_harness(Some(&path), &refused).is_none(),
            "a file gone before the re-read writes no policy — never the defaults"
        );
        assert!(
            observed_harness(Some(&path), &Err(DocumentHostError::NotAFile)).is_none(),
            "another refusal writes no policy"
        );
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
        /// `each` over `paths`, on four threads.
        fn on_four_threads(paths: &[std::path::PathBuf], each: impl Fn(&std::path::Path) + Sync) {
            std::thread::scope(|scope| {
                for chunk in paths.chunks(paths.len().div_ceil(4).max(1)) {
                    let each = &each;
                    scope.spawn(move || chunk.iter().for_each(|path| each(path)));
                }
            });
        }

        let dir = std::env::temp_dir().join(format!(
            "aterm-theme-watch-entry-cap-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Laid, and removed, by four threads (2026-10-06, gate speed). Beside
        // `F_FULLFSYNC` writers, as the gate runs this beside other binaries,
        // every create and unlink here waits for the volume's next commit, and
        // calls from several threads wait for the same one. Beside eight
        // spinners and three such writers this test took 27.1-30.1 s on one
        // thread and 13.9-14.4 s on four (1.6-1.8 CPU-s against 1.2-1.5); alone,
        // 0.22 s and 0.15 s. Four is the knee: laying and removing the 4,097
        // files took 19.8-20.9 s on one thread (0.6 CPU-s), 10.2-10.9 s on four
        // (0.9-1.0) and 9.5-9.6 s on eight (1.9-2.0). The stamp reads the
        // names and file types, which are the same whichever thread made each.
        let noise = (0..=crate::app_config::MAX_USER_THEME_DIRECTORY_ENTRIES)
            .map(|index| dir.join(format!("noise-{index:04}")))
            .collect::<Vec<_>>();
        on_four_threads(&noise, |path| std::fs::write(path, []).unwrap());
        let over = theme_directory_stamp(&dir).unwrap();
        assert!(over.truncated);
        assert_eq!(
            over.inspected_entries,
            crate::app_config::MAX_USER_THEME_DIRECTORY_ENTRIES + 1
        );
        std::fs::remove_file(&noise[0]).unwrap();
        let under = theme_directory_stamp(&dir).unwrap();
        assert_ne!(over, under, "returning under the entry cap is observable");
        assert!(!under.truncated);
        on_four_threads(&noise[1..], |path| {
            let _ = std::fs::remove_file(path);
        });
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
    fn theme_scans_back_off_only_after_a_stable_sample_and_follow_edges_quickly() {
        use std::time::Instant;

        let dir = std::env::temp_dir().join(format!(
            "aterm-theme-watch-idle-cadence-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let start = Instant::now();
        let empty = theme_directory_stamp(&dir).unwrap();
        let mut schedule = ThemePollSchedule::new(start, Some(Ok(empty.clone())));

        // The first reconciliation remains at the old 500 ms deadline.
        assert!(!schedule.due(start + POLL_INTERVAL - std::time::Duration::from_nanos(1)));
        let first = start + POLL_INTERVAL;
        assert!(schedule.due(first));
        schedule.observed(first, Ok(empty));
        assert!(!schedule.due(first + POLL_INTERVAL));
        let idle_due = first + THEME_IDLE_POLL_INTERVAL;
        assert!(schedule.due(idle_due));

        // An independent theme edit is observed at the bounded idle deadline;
        // its next sample gets the original 500 ms cadence.
        std::fs::write(dir.join("Work.conf"), "foreground = #112233\n").unwrap();
        let created = theme_directory_stamp(&dir).unwrap();
        schedule.observed(idle_due, Ok(created.clone()));
        assert!(!schedule.due(idle_due + POLL_INTERVAL - std::time::Duration::from_nanos(1)));
        let follow_up = idle_due + POLL_INTERVAL;
        assert!(schedule.due(follow_up));
        schedule.observed(follow_up, Ok(created));
        assert!(!schedule.due(follow_up + POLL_INTERVAL));
        assert!(schedule.due(follow_up + THEME_IDLE_POLL_INTERVAL));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn theme_failure_and_recovery_get_one_fast_follow_up_each() {
        use std::time::Instant;

        let start = Instant::now();
        let mut schedule = ThemePollSchedule::new(start, None);
        let failed_at = start + POLL_INTERVAL;
        schedule.observed(failed_at, Err(WatchFailureKind::ThemeDirectoryUnreadable));
        let repeated_at = failed_at + POLL_INTERVAL;
        assert!(schedule.due(repeated_at));
        schedule.observed(repeated_at, Err(WatchFailureKind::ThemeDirectoryUnreadable));
        assert!(!schedule.due(repeated_at + POLL_INTERVAL));
        let recovered_at = repeated_at + THEME_IDLE_POLL_INTERVAL;
        assert!(schedule.due(recovered_at));
        let recovered = super::ThemeDirectoryStamp {
            fingerprint: 1,
            inspected_entries: 0,
            candidate_metadata: 0,
            truncated: false,
        };
        schedule.observed(recovered_at, Ok(recovered));
        assert!(schedule.due(recovered_at + POLL_INTERVAL));
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
        assert_eq!(i64::from(latch.kind.is_some()), modeled["failed"]);
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
        assert_eq!(i64::from(latch.kind.is_some()), modeled["failed"]);
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
