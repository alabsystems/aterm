// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live config hot-reload watcher.

use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use aterm_window::event_loop::EventLoopProxy;

use crate::Wake;

#[cfg(clean_islands)]
#[path = "config_watcher_island.rs"]
mod island;

#[cfg(all(clean_islands, trust_seals))]
#[rustfmt::skip]
#[path = "config_watcher_protocol_island.rs"]
mod protocol_island;

#[cfg(all(clean_islands, trust_seals, trust_binding))]
#[path = "config_watcher_binding_island.rs"]
mod binding_island;

/// How often a watcher that hears no file events looks at the config file, and how soon one that
/// does looks again at a refusal no edit ends ([`Retry`]).
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// A stable theme directory needs a bounded rescan, but not four directory
/// walks per two seconds.
const THEME_IDLE_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// The independently recoverable host inputs watched by this worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum WatchTarget {
    Config,
    Themes,
}

/// Bounded user-facing failure classes.
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

/// One edge in the failure/recovery protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WatchStatusEvent {
    Failed(WatchFailure),
    Recovered(WatchTarget),
}

/// Process-global projection installed by the UI reducer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WatchStatusState {
    config: Option<WatchFailureKind>,
    themes: Option<WatchFailureKind>,
    /// Latest exact readable generation handed to the asynchronous config
    /// preparation/admission lane.
    config_candidate: Option<crate::native_document_host::AtomicFileBaseline>,
}

impl WatchStatusState {
    #[must_use]
    pub(crate) const fn has_config_candidate(&self) -> bool {
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

    /// Admit recovery only for the newest exact watcher generation.
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

    /// Reject only the exact watcher generation that is still current.
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
            if let Some(notice) = crate::app_config::launch_load_failure() {
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

const fn failure_message(kind: WatchFailureKind) -> &'static str {
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

/// Worker-side failure latch.
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

    const fn clear_silently(&mut self) {
        self.kind = None;
        self.unknown = false;
    }

    /// Forget what the window was told: its status wakes were dropped before
    /// Commit, so the next outcome — failure or recovery — is posted.
    const fn forget(&mut self) {
        self.kind = None;
        self.unknown = true;
    }
}

/// Spawn the single, process-wide config-watcher thread, `aterm-config-watch`.
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
    let started = std::thread::Builder::new()
        .name("aterm-config-watch".to_owned())
        .spawn(move || {
            let (ring, tells) = mpsc::channel();
            REPOST_AFTER_COMMIT.listen(ring.clone());
            let mut ears = platform_ears(ring, tells);
            watch(
                path,
                startup_config_baseline,
                initial_themes,
                theme_dir,
                &mut |wake| proxy.send_event(wake).is_ok(),
                ears.as_mut(),
                &REPOST_AFTER_COMMIT,
            );
        });
    if let Err(why) = started {
        aterm_log::info!("config watcher: its thread did not start ({why}), so no edit reloads");
    }
}

/// What wakes the watcher's rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tell {
    /// A held key's path changed ([`Held`]).
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "linux", test)),
        expect(dead_code, reason = "only a file watch tells a change")
    )]
    Changed(u64),
    /// A seamless successor reached Commit ([`Repost::raise`]).
    #[cfg_attr(
        not(any(unix, test)),
        expect(dead_code, reason = "only a unix successor's Commit raises a repost")
    )]
    Repost,
    /// The file watch's thread ended: from here on the watcher looks on its clock.
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "linux", test)),
        expect(dead_code, reason = "only a file watch's thread ends")
    )]
    EventsEnded,
}

/// How the watcher hears its paths change: a file watch ([`Heard`]), or nothing but its own
/// clock ([`Deaf`]). The laws drive [`watch`] through a scripted one.
trait Ears {
    fn watch(&mut self, key: u64, path: std::path::PathBuf);
    fn watch_directory(&mut self, key: u64, path: std::path::PathBuf);
    fn unwatch(&mut self, key: u64);
    /// Whether no change is heard, so every step runs on the clock.
    fn deaf(&self) -> bool;
    fn now(&self) -> Instant;
    /// Rest until a tell or `until` (none: until a tell), pushing every tell queued; the time it
    /// woke, or `None` once nothing can tell it again.
    fn wait(&mut self, until: Option<Instant>, tells: &mut Vec<Tell>) -> Option<Instant>;
}

/// [`Ears::wait`] over a channel of tells.
fn rest_on(
    tells: &mpsc::Receiver<Tell>,
    until: Option<Instant>,
    into: &mut Vec<Tell>,
) -> Option<Instant> {
    let first = match until {
        None => Some(tells.recv().ok()?),
        Some(until) => match tells.recv_timeout(until.saturating_duration_since(Instant::now())) {
            Ok(tell) => Some(tell),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
        },
    };
    into.extend(first);
    into.extend(tells.try_iter());
    Some(Instant::now())
}

/// This platform's file events, on their own thread `aterm-config-events`; where they do not
/// start, or off macOS and Linux, a clock alone ([`Deaf`]).
fn platform_ears(ring: mpsc::Sender<Tell>, tells: mpsc::Receiver<Tell>) -> Box<dyn Ears> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let ended = Ended(ring);
        let events = aterm_uds::file_watch::FileWatch::platform(
            "aterm-config-events",
            Box::new(move |key| {
                let Ended(ring) = &ended;
                let _ = ring.send(Tell::Changed(key));
            }),
        );
        match events {
            Ok(watch) => return Box::new(Heard { watch, tells }),
            Err(why) => {
                aterm_log::info!(
                    "config watcher: no file events ({why}), so the config is looked at every {POLL_INTERVAL:?}"
                );
                // The notify never ran: its `Ended`, dropped with it, told of an end no
                // watch had. A repost this takes too is still raised, and the loop takes it.
                tells.try_iter().for_each(drop);
            }
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    drop(ring);
    Box::new(Deaf { tells })
}

/// Owned by the file watch's notify: dropped as that watch's thread ends, it tells the watcher
/// so ([`Tell::EventsEnded`]).
#[cfg(any(target_os = "macos", target_os = "linux"))]
struct Ended(mpsc::Sender<Tell>);

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for Ended {
    fn drop(&mut self) {
        let _ = self.0.send(Tell::EventsEnded);
    }
}

/// The platform's file events ([`aterm_uds::file_watch::FileWatch`]).
#[cfg(any(target_os = "macos", target_os = "linux"))]
struct Heard {
    watch: aterm_uds::file_watch::FileWatch,
    tells: mpsc::Receiver<Tell>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Ears for Heard {
    fn watch(&mut self, key: u64, path: std::path::PathBuf) {
        self.watch.watch(key, path);
    }

    fn watch_directory(&mut self, key: u64, path: std::path::PathBuf) {
        self.watch.watch_directory(key, path);
    }

    fn unwatch(&mut self, key: u64) {
        self.watch.unwatch(key);
    }

    fn deaf(&self) -> bool {
        false
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wait(&mut self, until: Option<Instant>, tells: &mut Vec<Tell>) -> Option<Instant> {
        rest_on(&self.tells, until, tells)
    }
}

/// No file events (off macOS and Linux, or where they did not start): the config is looked at
/// every [`POLL_INTERVAL`] and the themes on their [`ThemePollSchedule`], and only a
/// [`Tell::Repost`] wakes the rest early.
struct Deaf {
    tells: mpsc::Receiver<Tell>,
}

impl Ears for Deaf {
    fn watch(&mut self, _key: u64, _path: std::path::PathBuf) {}

    fn watch_directory(&mut self, _key: u64, _path: std::path::PathBuf) {}

    fn unwatch(&mut self, _key: u64) {}

    fn deaf(&self) -> bool {
        true
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wait(&mut self, until: Option<Instant>, tells: &mut Vec<Tell>) -> Option<Instant> {
        rest_on(&self.tells, until, tells)
    }
}

/// The step a held key's change owes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owes {
    Config,
    Themes,
}

/// A path the watcher holds a key on, and how.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Hold {
    File(std::path::PathBuf),
    Directory(std::path::PathBuf),
}

/// The keys the watcher holds, each for one step. A key is never used twice, so a tell queued
/// before its path was let go cannot be taken for another path's.
#[derive(Default)]
struct Held {
    next: u64,
    keys: std::collections::BTreeMap<u64, (Owes, Hold)>,
}

impl Held {
    /// Hold `wanted` for `owes`: let go each of its keys whose path left the set and watch each
    /// path that came, keeping the rest (a path watched again would be told at its first look).
    fn hold(&mut self, ears: &mut dyn Ears, owes: Owes, mut wanted: BTreeSet<Hold>) {
        self.keys.retain(|key, (role, hold)| {
            if *role != owes || wanted.remove(hold) {
                return true;
            }
            ears.unwatch(*key);
            false
        });
        for hold in wanted {
            let key = self.next;
            self.next += 1;
            match &hold {
                Hold::File(path) => ears.watch(key, path.clone()),
                Hold::Directory(path) => ears.watch_directory(key, path.clone()),
            }
            self.keys.insert(key, (owes, hold));
        }
    }

    fn owes(&self, key: u64) -> Option<Owes> {
        self.keys.get(&key).map(|(owes, _)| *owes)
    }
}

/// What the config step reads: the config file and each path feed, through every link a lookup
/// of each takes ([`aterm_uds::file_watch::link_chain`]), so an edit of a file a link names in
/// another folder is told too.
fn config_holds(path: &std::path::Path, feeds: &[std::path::PathBuf]) -> BTreeSet<Hold> {
    std::iter::once(path)
        .chain(feeds.iter().map(std::path::PathBuf::as_path))
        .flat_map(aterm_uds::file_watch::link_chain)
        .map(Hold::File)
        .collect()
}

/// What the theme step reads: the theme folder (its entries), the links a lookup of it takes,
/// and each theme file discovery reads the bytes of, which can change with no entry changed: of
/// the `candidates` the stamp read, in name order, the first
/// [`crate::app_config::MAX_USER_THEME_FILES`] whose names discovery takes
/// ([`crate::app_config::user_theme_file_read`]). Which files those are changes only with the
/// folder's entries, which its key hears.
fn theme_holds(directory: &std::path::Path, candidates: &[std::path::PathBuf]) -> BTreeSet<Hold> {
    let mut chain = aterm_uds::file_watch::link_chain(directory);
    let folder = chain.pop().map(Hold::Directory);
    let read = candidates
        .iter()
        .filter(|candidate| crate::app_config::user_theme_file_read(candidate))
        .take(crate::app_config::MAX_USER_THEME_FILES)
        .cloned()
        .map(Hold::File);
    chain
        .into_iter()
        .map(Hold::File)
        .chain(folder)
        .chain(read)
        .collect()
}

/// The longest a failure no edit ends waits for its next look ([`Retry`]).
const RETRY_MOST: Duration = Duration::from_secs(10);

/// When a step whose last failure no edit ends ([`ends_with_no_edit`]) looks again: after
/// [`POLL_INTERVAL`], then twice as long each time it fails so, up to [`RETRY_MOST`]. A success,
/// or a failure an edit ends, sets no clock.
#[derive(Default)]
struct Retry {
    at: Option<Instant>,
    every: Duration,
}

impl Retry {
    fn due(&self, now: Instant) -> bool {
        self.at.is_some_and(|at| now >= at)
    }

    fn after(&mut self, now: Instant, failure: Option<WatchFailureKind>) {
        if failure.is_some_and(ends_with_no_edit) {
            self.every = if self.at.is_some() {
                self.every.saturating_mul(2).min(RETRY_MOST)
            } else {
                POLL_INTERVAL
            };
            self.at = Some(now + self.every);
        } else {
            self.at = None;
        }
    }
}

/// Whether a failure of `kind` can end with no event the watch hears, so the step looks again on
/// a clock: a refusal or an I/O error (a privacy grant lifts one unseen). An edit ends the rest,
/// and its event wakes the step: bytes that do not read, a file too large or not a file, a
/// binding through an unsafe link, or a change made as it read.
const fn ends_with_no_edit(kind: WatchFailureKind) -> bool {
    match kind {
        WatchFailureKind::ConfigUnreadable
        | WatchFailureKind::ThemeDirectoryUnreadable
        | WatchFailureKind::ThemeEntryUnreadable
        | WatchFailureKind::ThemeFileUnavailable => true,
        WatchFailureKind::ConfigInvalidUtf8
        | WatchFailureKind::ConfigInvalidToml
        | WatchFailureKind::ConfigTooLarge
        | WatchFailureKind::ConfigNotRegular
        | WatchFailureKind::ConfigChangedWhileReading
        | WatchFailureKind::ConfigUnsafeBinding
        | WatchFailureKind::ConfigPreparationFailed
        | WatchFailureKind::ThemeChangedWhileScanning => false,
    }
}

/// The clock of a watcher that hears nothing: the config every [`POLL_INTERVAL`], the themes on
/// their schedule.
struct DeafClock {
    config_at: Instant,
    themes: ThemePollSchedule,
}

impl DeafClock {
    fn new(now: Instant) -> Self {
        Self {
            config_at: now,
            themes: ThemePollSchedule::new(now, None),
        }
    }
}

/// The earliest of `clocks` that is set.
fn earliest(clocks: impl IntoIterator<Item = Option<Instant>>) -> Option<Instant> {
    clocks.into_iter().flatten().min()
}

/// The config-watcher thread [`spawn`] starts. It reads the config and the themes once, holds a
/// key on each path those reads depend on, and then rests until one is told changed, a seamless
/// successor's Commit asks for a repost, or a failure no edit ends is due a look; a step whose
/// stamps did not move reads no bytes. Every post goes through `post`; one refused (the event
/// loop is gone) ends the thread.
fn watch(
    path: Option<std::path::PathBuf>,
    startup_config_baseline: Option<crate::native_document_host::AtomicFileBaseline>,
    initial_themes: Arc<crate::app_config::ThemeCatalog>,
    theme_dir: Option<std::path::PathBuf>,
    post: &mut dyn FnMut(Wake) -> bool,
    ears: &mut dyn Ears,
    repost: &Repost,
) {
    let mut held = Held::default();
    // Held before the first read, so an edit after the read is heard; the feeds come with it.
    if let Some(path) = path.as_deref() {
        held.hold(ears, Owes::Config, config_holds(path, &[]));
    }
    let mut last_config_stamp = path
        .as_deref()
        .and_then(|path| config_path_stamp(path).ok());
    let initial_config = path.as_deref().map(config_file_observation);
    let mut feed_paths = match initial_config.as_ref() {
        Some(Ok(observation)) => feed_paths_of(&observation.text).unwrap_or_default(),
        _ => Vec::new(),
    };
    let mut last_feed_stamp = path_feed_stamp(&feed_paths);
    let mut config_needs_retry = initial_config.as_ref().is_some_and(Result::is_err);
    let mut config_failure = FailureLatch::default();
    let mut last_harness = None;
    let mut harness_stamps = crate::harness_policy_cell::HarnessStampSource::new();
    if let Some(initial) = initial_config.as_ref()
        && let Some(policy) = observed_harness(path.as_deref(), initial)
        && !post_harness(
            send_harness(post),
            &mut last_harness,
            &mut harness_stamps,
            policy,
        )
    {
        return;
    }
    if let Some(Err(error)) = initial_config.as_ref()
        && !post_status(
            post,
            &mut config_failure,
            WatchTarget::Config,
            Some(config_failure_kind(error)),
        )
    {
        return;
    }
    let mut now = ears.now();
    let mut config_retry = Retry::default();
    if let Some(Err(error)) = initial_config.as_ref() {
        config_retry.after(now, Some(config_failure_kind(error)));
    }
    if let Some(Ok(observation)) = initial_config
        && initial_config_observation_changed(startup_config_baseline.as_ref(), &observation)
        && !post(Wake::ConfigReloadObserved(observation))
    {
        return;
    }
    if let Some(path) = path.as_deref() {
        held.hold(ears, Owes::Config, config_holds(path, &feed_paths));
    }

    let initial_theme_stamp = theme_dir.as_deref().map(theme_directory_stamp);
    let mut last_theme_stamp = None;
    let mut theme_failure = FailureLatch::default();
    if let Some(Err(kind)) = initial_theme_stamp.as_ref()
        && !post_status(post, &mut theme_failure, WatchTarget::Themes, Some(*kind))
    {
        return;
    }
    let mut deaf = ears.deaf().then(|| DeafClock {
        config_at: now + POLL_INTERVAL,
        themes: ThemePollSchedule::new(now, initial_theme_stamp.clone()),
    });
    let mut themes = initial_themes;
    let mut theme_repost = false;
    let mut theme_retry = Retry::default();
    let mut theme_candidates = Vec::new();
    // The first theme step reads the catalog as it stands and finds what to hold.
    let mut themes_owed = deaf.is_none();
    let mut config_owed = false;
    let mut tells = Vec::new();
    loop {
        if forget_after_commit(repost, &mut last_harness) {
            config_failure.forget();
            config_needs_retry = true;
            theme_failure.forget();
            last_theme_stamp = None;
            theme_repost = true;
            config_owed = true;
            themes_owed = true;
        }
        if let Some(path) = path.as_deref()
            && (std::mem::take(&mut config_owed)
                || config_retry.due(now)
                || deaf.as_ref().is_some_and(|clock| now >= clock.config_at))
        {
            let mut failed = None;
            match config_path_stamp(path) {
                Err(kind) => {
                    failed = Some(kind);
                    config_needs_retry = true;
                    if !post_stamp_refusal(
                        send_harness(post),
                        &mut last_harness,
                        &mut harness_stamps,
                        path,
                    ) {
                        return;
                    }
                    if !post_status(post, &mut config_failure, WatchTarget::Config, Some(kind)) {
                        return;
                    }
                }
                Ok(stamp) => {
                    let feeds_moved = path_feed_stamp(&feed_paths) != last_feed_stamp;
                    let candidate = prepare_config_edge_with(
                        last_config_stamp.as_ref(),
                        stamp,
                        config_needs_retry || feeds_moved,
                        || config_file_observation(path),
                    );
                    if let Some((stamp, observation)) = candidate {
                        // The policy first, for a refused file as for a readable one: the
                        // window runs the newest read's `[harness]`, and a readable
                        // observation's admission reports under it.
                        if let Some(policy) = observed_harness(Some(path), &observation)
                            && !post_harness(
                                send_harness(post),
                                &mut last_harness,
                                &mut harness_stamps,
                                policy,
                            )
                        {
                            return;
                        }
                        match observation {
                            Ok(observation) => {
                                // Re-derive the feed set from this text and stamp it
                                // before posting, so an edit racing the UI's read is
                                // the next step's edge. An unparseable config keeps
                                // the last known set (admission rejects the text).
                                if let Some(paths) = feed_paths_of(&observation.text) {
                                    feed_paths = paths;
                                }
                                last_feed_stamp = path_feed_stamp(&feed_paths);
                                if !post(Wake::ConfigReloadObserved(observation)) {
                                    return;
                                }
                                config_failure.clear_silently();
                                config_needs_retry = false;
                                last_config_stamp = Some(stamp);
                            }
                            Err(error) => {
                                let kind = config_failure_kind(&error);
                                failed = Some(kind);
                                config_needs_retry = true;
                                if !post_status(
                                    post,
                                    &mut config_failure,
                                    WatchTarget::Config,
                                    Some(kind),
                                ) {
                                    return;
                                }
                            }
                        }
                    }
                }
            }
            config_retry.after(now, failed);
            if let Some(clock) = deaf.as_mut() {
                clock.config_at = now + POLL_INTERVAL;
            }
            held.hold(ears, Owes::Config, config_holds(path, &feed_paths));
        }

        if let Some(directory) = theme_dir.as_deref()
            && (std::mem::take(&mut themes_owed)
                || theme_retry.due(now)
                || deaf.as_ref().is_some_and(|clock| clock.themes.due(now)))
        {
            let sample = match theme_directory_read(directory) {
                Err(kind) => {
                    if !post_status(post, &mut theme_failure, WatchTarget::Themes, Some(kind)) {
                        return;
                    }
                    Err(kind)
                }
                Ok((stamp, candidates)) => {
                    // A read that fails keeps the files the last one held.
                    theme_candidates = candidates;
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
                            if !post_status(post, &mut theme_failure, WatchTarget::Themes, None) {
                                return;
                            }
                            Ok(sampled_stamp)
                        }
                        Some(Err(kind)) => {
                            if !post_status(
                                post,
                                &mut theme_failure,
                                WatchTarget::Themes,
                                Some(kind),
                            ) {
                                return;
                            }
                            Err(kind)
                        }
                        Some(Ok((stable_stamp, discovered))) => {
                            if theme_repost || *discovered != *themes {
                                if !post(Wake::ThemeCatalogChanged(Arc::clone(&discovered))) {
                                    return;
                                }
                                themes = discovered;
                                theme_repost = false;
                            }
                            last_theme_stamp = Some(stable_stamp.clone());
                            if !post_status(post, &mut theme_failure, WatchTarget::Themes, None) {
                                return;
                            }
                            Ok(stable_stamp)
                        }
                    }
                }
            };
            theme_retry.after(now, sample.as_ref().err().copied());
            if let Some(clock) = deaf.as_mut() {
                clock.themes.observed(now, sample);
            }
            held.hold(
                ears,
                Owes::Themes,
                theme_holds(directory, &theme_candidates),
            );
        }

        let until = earliest([
            config_retry.at,
            theme_retry.at,
            deaf.as_ref()
                .filter(|_| path.is_some())
                .map(|clock| clock.config_at),
            deaf.as_ref()
                .filter(|_| theme_dir.is_some())
                .map(|clock| clock.themes.next_poll),
        ]);
        let Some(woke) = ears.wait(until, &mut tells) else {
            return;
        };
        now = woke;
        for tell in tells.drain(..) {
            match tell {
                Tell::Changed(key) => match held.owes(key) {
                    Some(Owes::Config) => config_owed = true,
                    Some(Owes::Themes) => themes_owed = true,
                    None => {}
                },
                // The flag it raised is taken at the loop's top.
                Tell::Repost => {}
                Tell::EventsEnded => {
                    deaf.get_or_insert_with(|| DeafClock::new(now));
                    config_owed = true;
                    themes_owed = true;
                }
            }
        }
    }
}

/// The `[harness]` policy of one watcher read: this thread is the one writer of the policy the
/// window runs (`App::adopt_harness_policy` handles only its wake).
fn observed_harness(
    path: Option<&std::path::Path>,
    observation: &Result<
        crate::native_config_service::ConfigDiskObservation,
        crate::native_document_host::DocumentHostError,
    >,
) -> Option<crate::app_config::HarnessPolicy> {
    use crate::native_document_host::DocumentHostError;
    match observation {
        Ok(observation) => Some(crate::app_config::HarnessPolicy::read(&observation.text)),
        Err(DocumentHostError::InvalidUtf8 | DocumentHostError::TooLarge { .. }) => {
            // Keep owned lossy-comment policy reads, with the same admission
            // and size cap as every other file-backed harness reader.
            Some(match path {
                Some(path) => crate::app_config::HarnessPolicy::read_path(Some(path)),
                None => unread_harness("failed config observation has no file context".into()),
            })
        }
        Err(error) => Some(unread_harness(format!(
            "config observation failed: {error:?}"
        ))),
    }
}

fn unread_harness(note: String) -> crate::app_config::HarnessPolicy {
    let (policy, notes) = aterm_agent::supervise::SupervisorConfig::unread_config(note);
    crate::app_config::HarnessPolicy {
        policy,
        notes,
        owner: Default::default(),
    }
}

fn post_stamp_refusal(
    send: impl FnOnce(crate::harness_policy_cell::StampedHarnessPolicy) -> bool,
    last: &mut Option<crate::app_config::HarnessPolicy>,
    stamps: &mut crate::harness_policy_cell::HarnessStampSource,
    path: &std::path::Path,
) -> bool {
    post_harness(
        send,
        last,
        stamps,
        unread_harness(format!(
            "{}: config metadata cannot be read",
            path.display()
        )),
    )
}

/// Post `policy` as the one the window runs when it differs from the one last
/// posted (`last`), recording it only once `send` queued the wake
/// (`Wake::HarnessPolicyObserved`).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
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
    post: &mut dyn FnMut(Wake) -> bool,
) -> impl FnOnce(crate::harness_policy_cell::StampedHarnessPolicy) -> bool + '_ {
    move |policy| post(Wake::HarnessPolicyObserved(policy))
}

/// A seamless successor's Commit, for the watcher: every config wake before it was dropped
/// (`App::drops_wake_before_commit`), so the watcher's next step posts the file again. `raised`
/// is the request, which [`forget_after_commit`] takes; `ring` wakes the watcher's rest for it.
struct Repost {
    raised: std::sync::atomic::AtomicBool,
    ring: std::sync::Mutex<Option<mpsc::Sender<Tell>>>,
}

impl Repost {
    const fn new() -> Self {
        Self {
            raised: std::sync::atomic::AtomicBool::new(false),
            ring: std::sync::Mutex::new(None),
        }
    }

    /// Wake the watcher resting on `ring` at each [`Self::raise`].
    fn listen(&self, ring: mpsc::Sender<Tell>) {
        *self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ring);
    }

    #[cfg(any(unix, test))]
    fn raise(&self) {
        self.raised
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(ring) = self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            let _ = ring.send(Tell::Repost);
        }
    }

    fn take(&self) -> bool {
        self.raised
            .swap(false, std::sync::atomic::Ordering::Relaxed)
    }
}

/// The running watcher's [`Repost`] ([`repost_after_commit`]).
static REPOST_AFTER_COMMIT: Repost = Repost::new();

/// The successor reached Commit: every config wake before it was dropped (`App::drops_wake_before_commit`), so the watcher posts the file again.
#[cfg(unix)]
pub(crate) fn repost_after_commit() {
    REPOST_AFTER_COMMIT.raise();
}

/// The protocol's Forget: once a seamless successor's Commit raised `repost`, the watcher
/// forgets the policy it posted, so its next read posts again what the window dropped.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessPolicyOneWriter",
        action = "Forget",
        project = "aterm_gui::config_watcher::tests::project_harness_writer"
    )
)]
fn forget_after_commit(
    repost: &Repost,
    last: &mut Option<crate::app_config::HarnessPolicy>,
) -> bool {
    if !repost.take() {
        return false;
    }
    *last = None;
    true
}

fn post_status(
    post: &mut dyn FnMut(Wake) -> bool,
    latch: &mut FailureLatch,
    target: WatchTarget,
    failure: Option<WatchFailureKind>,
) -> bool {
    latch
        .transition(target, failure)
        .is_none_or(|event| post(Wake::ConfigWatchStatus(event)))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfigPathStamp(u64);

/// The path feeds a config text names (`Config::path_feed_paths`), or `None`
/// when the text does not parse.
fn feed_paths_of(text: &str) -> Option<Vec<std::path::PathBuf>> {
    crate::app_config::Config::parse(text)
        .ok()
        .map(|config| config.path_feed_paths())
}

/// Metadata-only stamp over the path feeds: the logical entry, a symlink's target and the followed file for each, or its absence.
pub(crate) fn path_feed_stamp(paths: &[std::path::PathBuf]) -> u64 {
    path_feed_stamp_with(paths, |path| std::fs::metadata(path))
}

/// [`path_feed_stamp`] with the following `stat` injected, so a test counts
/// the calls.
fn path_feed_stamp_with(
    paths: &[std::path::PathBuf],
    mut stat: impl FnMut(&std::path::Path) -> std::io::Result<std::fs::Metadata>,
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "aterm-path-feed-metadata-v1".hash(&mut hasher);
    for path in paths {
        path.hash(&mut hasher);
        let logical = std::fs::symlink_metadata(path);
        match &logical {
            Ok(logical) => {
                hash_file_metadata(logical, &mut hasher);
                if logical.file_type().is_symlink() {
                    std::fs::read_link(path).ok().hash(&mut hasher);
                }
            }
            Err(_) => "absent".hash(&mut hasher),
        }
        let follow = cfg!(not(unix)) || logical.as_ref().is_ok_and(|l| l.file_type().is_symlink());
        let followed = if follow { stat(path) } else { logical };
        match followed {
            Ok(followed) => hash_file_metadata(&followed, &mut hasher),
            Err(_) => "target-absent".hash(&mut hasher),
        }
    }
    hasher.finish()
}

/// Run the exact content observation only for a metadata edge or a prior
/// observation failure.
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

/// Cheap identity/metadata stamp for the config's logical path.
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
            "followed-target-absent".hash(&mut hasher);
        }
        Err(_) => return Err(WatchFailureKind::ConfigUnreadable),
    }
    Ok(ConfigPathStamp(hasher.finish()))
}

/// One safe, bounded generation observation of the config's logical path, through the
/// config/Manual file-capability reader: its baseline includes a content fingerprint,
/// regular-file identity, metadata token and the complete admitted symlink binding.
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

const fn config_failure_kind(
    error: &crate::native_document_host::DocumentHostError,
) -> WatchFailureKind {
    use crate::native_document_host::DocumentHostError;
    match error {
        DocumentHostError::InvalidUtf8 => WatchFailureKind::ConfigInvalidUtf8,
        DocumentHostError::TooLarge { .. } => WatchFailureKind::ConfigTooLarge,
        DocumentHostError::NotAFile => WatchFailureKind::ConfigNotRegular,
        DocumentHostError::ChangedWhileReading => WatchFailureKind::ConfigChangedWhileReading,
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
/// observed theme change or failure transition.
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

/// Cheap bounded identity/metadata stamp for the theme directory.
fn theme_directory_stamp(path: &std::path::Path) -> Result<ThemeDirectoryStamp, WatchFailureKind> {
    theme_directory_stamp_capped(path, crate::app_config::ThemeScanCaps::PRODUCTION)
}

/// [`theme_directory_stamp`], and the theme files it stamped, in name order: the watcher holds
/// a key on those discovery reads ([`theme_holds`]).
fn theme_directory_read(
    path: &std::path::Path,
) -> Result<(ThemeDirectoryStamp, Vec<std::path::PathBuf>), WatchFailureKind> {
    theme_directory_read_capped(path, crate::app_config::ThemeScanCaps::PRODUCTION)
}

/// [`theme_directory_stamp`] reading at most `caps.entries` entries (`truncated`
/// past it) and keeping the metadata of at most `caps.candidates` candidates —
/// discovery's own bounds ([`crate::app_config::ThemeScanCaps`]).
fn theme_directory_stamp_capped(
    path: &std::path::Path,
    caps: crate::app_config::ThemeScanCaps,
) -> Result<ThemeDirectoryStamp, WatchFailureKind> {
    theme_directory_read_capped(path, caps).map(|(stamp, _)| stamp)
}

/// [`theme_directory_stamp_capped`], and the candidates whose metadata it stamped.
fn theme_directory_read_capped(
    path: &std::path::Path,
    caps: crate::app_config::ThemeScanCaps,
) -> Result<(ThemeDirectoryStamp, Vec<std::path::PathBuf>), WatchFailureKind> {
    let cap = caps.entries;
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
        for (index, result) in entries.take(cap.saturating_add(1)).enumerate() {
            inspected_entries = inspected_entries.saturating_add(1);
            if index == cap {
                truncated = true;
                break;
            }
            let entry = result.map_err(|_| WatchFailureKind::ThemeEntryUnreadable)?;
            let file_type = entry.file_type().map_err(theme_entry_failure_kind)?;
            if !file_type.is_file()
                || std::path::Path::new(&entry.file_name()).extension()
                    != Some(std::ffi::OsStr::new("conf"))
            {
                continue;
            }
            count = count.saturating_add(1);
            candidates.insert(entry.path());
            if candidates.len() > caps.candidates {
                candidates.pop_last();
            }
        }
    }
    count.hash(&mut hasher);
    inspected_entries.hash(&mut hasher);
    truncated.hash(&mut hasher);
    let candidate_metadata = candidates.len();
    let mut stamped = Vec::with_capacity(candidate_metadata);
    for candidate in candidates {
        candidate.hash(&mut hasher);
        let metadata = std::fs::symlink_metadata(&candidate).map_err(theme_entry_failure_kind)?;
        hash_file_metadata(&metadata, &mut hasher);
        stamped.push(candidate);
    }
    Ok((
        ThemeDirectoryStamp {
            fingerprint: hasher.finish(),
            inspected_entries,
            candidate_metadata,
            truncated,
        },
        stamped,
    ))
}

fn theme_entry_failure_kind(error: std::io::Error) -> WatchFailureKind {
    if error.kind() == std::io::ErrorKind::NotFound {
        WatchFailureKind::ThemeChangedWhileScanning
    } else {
        WatchFailureKind::ThemeEntryUnreadable
    }
}

const fn theme_discovery_failure_kind(
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
    pub(crate) fn apply_config_watch_status(&mut self, event: WatchStatusEvent) {
        if self.native_runtime.apply_config_watch_status(event) {
            self.request_redraw_all_windows();
        }
    }
}

/// `HarnessPolicyOneWriter` actions no shipping function performs, each waived where
/// its Tier-1 (`tests`) binds it.
#[cfg(any(test, debug_assertions))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessPolicyOneWriter",
    action = "WriteOn",
    reason = "The environment: a person saves aterm.toml with approvals on. The Tier-1 \
              writes the real file and the real watcher read (observed_harness) sees it."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessPolicyOneWriter",
    action = "WriteOff",
    reason = "The environment: a person saves the kill switch, refused for a stray \
              non-UTF-8 byte. The Tier-1 writes the real file and the real watcher read \
              (observed_harness) sees it."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessPolicyOneWriter",
    action = "Commit",
    reason = "A seamless successor's Commit (Wake::ActivateCommittedHandoff), bound by the \
              native-update handoff machines. Its one effect here, repost_after_commit, \
              raises the process-wide REPOST_AFTER_COMMIT that a live watcher thread in \
              the same test binary could consume, so the Tier-1 raises a Repost of its own."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessPolicyOneWriter",
    action = "SecondWriterOff",
    reason = "The Buggy=1 mutant: an admission adopting its own kill switch, the design \
              this replaced; no shipping code, and the Tier-1's negative control sees the \
              step refused."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessPolicyOneWriter",
    action = "SecondWriterOn",
    reason = "The Buggy=1 mutant: an admission adopting its own approvals, the design this \
              replaced; no shipping code, and the Tier-1's negative control sees the step \
              refused."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessPolicyOneWriter",
    action = "CommitWithoutForget",
    reason = "The Buggy=1 mutant: a Commit that raises no REPOST_AFTER_COMMIT; no shipping \
              code (the Commit handler calls repost_after_commit)."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn harness_policy_one_writer_waivers() {}

#[cfg(test)]
pub(crate) mod tests {
    #[cfg(unix)]
    use super::feed_paths_of;
    use super::post_harness;
    use super::{
        FailureLatch, POLL_INTERVAL, THEME_IDLE_POLL_INTERVAL, ThemePollSchedule, WatchFailure,
        WatchFailureKind, WatchStatusEvent, WatchStatusState, WatchTarget,
        coherent_theme_sample_with, config_failure_kind, config_file_observation,
        config_file_stamp, config_path_stamp, initial_config_observation_changed, observed_harness,
        path_feed_stamp, prepare_config_edge_with, prepare_theme_edge_with, theme_directory_stamp,
        theme_directory_stamp_capped,
    };

    /// ONE `stat` PER PATH THAT IS NOT A SYMLINK (unix): a regular file, a
    /// folder and an absent path are hashed from their `lstat` alone, a
    /// symlink and a dangling one are followed, and the stamp is the value the
    /// two calls per path gave before, so the warm spare's learned gate, keyed
    /// by a generation stamped with it and persisted, survives the update.
    #[cfg(unix)]
    #[test]
    fn the_feed_stamp_follows_only_symlinks_and_keeps_its_value() {
        use std::hash::{Hash as _, Hasher as _};
        let dir = std::env::temp_dir().join(format!("aterm-feed-stamp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("folder")).unwrap();
        std::fs::write(dir.join("file"), b"feed").unwrap();
        std::os::unix::fs::symlink(dir.join("file"), dir.join("link")).unwrap();
        std::os::unix::fs::symlink(dir.join("nowhere"), dir.join("dangling")).unwrap();
        let paths: Vec<std::path::PathBuf> = ["file", "folder", "link", "dangling", "absent"]
            .iter()
            .map(|name| dir.join(name))
            .collect();
        let mut follows = 0;
        let stamp = super::path_feed_stamp_with(&paths, |path| {
            follows += 1;
            std::fs::metadata(path)
        });
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        "aterm-path-feed-metadata-v1".hash(&mut hasher);
        for path in &paths {
            path.hash(&mut hasher);
            match std::fs::symlink_metadata(path) {
                Ok(logical) => {
                    super::hash_file_metadata(&logical, &mut hasher);
                    if logical.file_type().is_symlink() {
                        std::fs::read_link(path).ok().hash(&mut hasher);
                    }
                }
                Err(_) => "absent".hash(&mut hasher),
            }
            match std::fs::metadata(path) {
                Ok(followed) => super::hash_file_metadata(&followed, &mut hasher),
                Err(_) => "target-absent".hash(&mut hasher),
            }
        }
        let production = path_feed_stamp(&paths);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            follows, 2,
            "only the symlink and the dangling one are followed"
        );
        assert_eq!(stamp, hasher.finish(), "the stamp two calls per path gave");
        assert_eq!(production, stamp, "the production stamp is the same");
    }

    /// Return once the clock the filesystem stamps a ctime from has moved past
    /// `path`'s ctime, so a rewrite now carries a ctime of its own.
    #[cfg(unix)]
    fn after_ctime_tick(path: &std::path::Path) {
        use std::os::unix::fs::MetadataExt;
        let ctime = |p: &std::path::Path| {
            let m = std::fs::metadata(p).unwrap();
            (m.ctime(), m.ctime_nsec())
        };
        let was = ctime(path);
        let probe = std::env::temp_dir().join(format!(
            "aterm-ctime-tick-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            std::fs::write(&probe, b"").unwrap();
            if ctime(&probe) > was {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the filesystem's clock did not move past a ctime in a minute"
            );
            aterm_uds::deadline::poll_yield();
        }
        let _ = std::fs::remove_file(&probe);
    }

    /// Editing only the lexicon hot-reloads it: the feeds the config names are stamped beside it,
    /// so a same-length, mtime-restored lexicon rewrite under an unchanged `aterm.toml` moves the
    /// feed stamp and re-posts the observation.
    #[cfg(unix)]
    #[test]
    fn a_lexicon_edit_under_an_unchanged_config_is_a_reload_edge() {
        let dir =
            std::env::temp_dir().join(format!("aterm-config-watch-lexicon-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("aterm.toml");
        let lexicon = dir.join("lexicon.toml");
        let toy = dir.join("toy.toml");
        std::fs::write(&lexicon, b"[[entry]]\nword = \"purr\"\n").unwrap();
        crate::test_scratch::write_config(
            &config,
            format!(
                "[sparkle_words]\nlexicon = {:?}\ntoy_packs = [{:?}]\n",
                lexicon.display().to_string(),
                toy.display().to_string(),
            ),
        );
        let observation = config_file_observation(&config).unwrap();
        let feeds = feed_paths_of(&observation.text).expect("the config parses");
        assert_eq!(feeds, vec![lexicon.clone(), toy.clone()]);
        let config_stamp = config_path_stamp(&config).unwrap();
        let before = path_feed_stamp(&feeds);
        assert_eq!(path_feed_stamp(&feeds), before, "control: idle is no edge");

        let metadata = std::fs::metadata(&lexicon).unwrap();
        let modified = metadata.modified().unwrap();
        after_ctime_tick(&lexicon);
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

        let absent = path_feed_stamp(&feeds);
        std::fs::write(&toy, b"").unwrap();
        assert_ne!(path_feed_stamp(&feeds), absent);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A disabled Sparkle table names no feed (it has no consumer), and the
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
            "[sparkle_words]\ntoy_packs = [{}]\n",
            many.join(", ")
        ))
        .unwrap();
        assert_eq!(packs.path_feed_paths().len(), 8, "the loaders' cap");

        let scratch = aterm_tempfile::Builder::new()
            .prefix("aterm-config-watch-ai-fleet-")
            .tempdir()
            .unwrap();
        let dir = scratch.path();
        let fleet = dir.join(aterm_agent::ai::config::FLEET_FILE);
        let read = || crate::app_config::Config {
            ai: crate::app_config::AiPolicy::read_beside("", Some(dir)),
            ..crate::app_config::Config::default()
        };
        let absent = read();
        assert!(matches!(
            absent.ai.settings().fleet,
            aterm_agent::ai::config::FleetState::None
        ));
        let feeds = absent.path_feed_paths();
        assert_eq!(feeds, vec![fleet.clone()]);
        let missing_stamp = path_feed_stamp(&feeds);
        assert_eq!(path_feed_stamp(&feeds), missing_stamp, "idle is no edge");

        crate::test_scratch::write_config(&fleet, "schema = 1\npreferred = \"codex\"\n");
        assert_ne!(
            path_feed_stamp(&feeds),
            missing_stamp,
            "creation is an edge"
        );
        let present = read();
        assert_eq!(present.ai.settings().preferred.0.id, "codex");
        let present_stamp = path_feed_stamp(&present.path_feed_paths());
        std::fs::remove_file(&fleet).unwrap();
        assert_ne!(
            path_feed_stamp(&feeds),
            present_stamp,
            "deletion is an edge"
        );
        let deleted = read();
        assert_eq!(
            deleted.path_feed_paths(),
            feeds,
            "absence retains the watch"
        );
        assert!(matches!(
            deleted.ai.settings().fleet,
            aterm_agent::ai::config::FleetState::None
        ));
        let explicit = crate::app_config::AiPolicy::read_beside(
            &format!("[ai]\nfleet = {:?}\n", fleet.display().to_string()),
            Some(dir),
        );
        assert!(
            matches!(
                explicit.settings().fleet,
                aterm_agent::ai::config::FleetState::Refused { .. }
            ),
            "an explicitly requested missing document is still refused"
        );
        let missing_stamp = path_feed_stamp(&deleted.path_feed_paths());
        crate::test_scratch::write_config(&fleet, "schema = 1\npreferred = \"agy\"\n");
        assert_ne!(
            path_feed_stamp(&feeds),
            missing_stamp,
            "recreation is an edge"
        );
        assert_eq!(read().ai.settings().preferred.0.id, "agy");
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
        crate::test_scratch::write_config(&path, "theme = \"Default\"\n");
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
        crate::test_scratch::write_config(&path, b"theme = \"Nord\"\n");
        let metadata = std::fs::metadata(&path).unwrap();
        let modified = metadata.modified().unwrap();
        let accessed = metadata.accessed().unwrap();
        let before = config_path_stamp(&path).unwrap();

        after_ctime_tick(&path);
        crate::test_scratch::write_config(&path, b"theme = \"Rose\"\n");
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
    /// what it handles yet, and the Commit's pending forget.
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
        /// The Commit's request to the watcher: raised, its forget is pending.
        repost: super::Repost,
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
                self.repost
                    .raised
                    .load(std::sync::atomic::Ordering::Relaxed),
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

        /// The file changes on disk: the kill switch goes in refused (a stray
        /// non-UTF-8 byte), approvals go in readable.
        fn write(&mut self, approve: bool) {
            let action = if approve { "WriteOn" } else { "WriteOff" };
            self.step(action, |d| {
                if approve {
                    crate::test_scratch::write_config(
                        &d.path,
                        "font_px = 14\n[harness]\napprove = \"all\"\n",
                    );
                } else {
                    crate::test_scratch::write_config(
                        &d.path,
                        b"# caf\xe9\n[harness]\nenabled = false\n",
                    );
                }
                d.disk = i64::from(approve);
                d.edits += 1;
            });
        }

        /// One watcher read of the file and its post: the real
        /// `observed_harness` and `post_harness`, whose send queues the wake.
        fn poll(&mut self) {
            if self.queued.is_some() {
                return;
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

        /// The config lane admits a generation carrying its own parse — the
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
                crate::test_scratch::write_config(&staged, text);
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
                        font_refusals: Vec::new(),
                        reason: crate::app_colorscheme::ThemeChange::ConfigReload,
                    },
                );
            });
        }

        /// A seamless successor's Commit: the window runs what it handles from
        /// here on, and the handler raises the watcher's `Repost`.
        fn commit(&mut self) {
            self.step("Commit", |d| {
                d.committed = true;
                d.repost.raise();
            });
        }

        /// The watcher's next step takes the request and forgets what it posted: the real
        /// `forget_after_commit`.
        fn forget(&mut self) {
            self.step("Forget", |d| {
                super::forget_after_commit(&d.repost, &mut d.last);
            });
        }
    }

    /// Tier-1 for `HarnessPolicyOneWriter`: drives the real read (`observed_harness`), post and
    /// dedupe (`post_harness`), the window's handler and the admission path through writes of a
    /// refused kill switch and a readable approval, polls around a seamless Commit, and stale
    /// admissions of both policies at the worst moments, validating every transition and
    /// invariant against the derived model.
    #[test]
    fn the_watcher_is_the_policys_one_writer() {
        let dir =
            std::env::temp_dir().join(format!("aterm-harness-one-writer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        crate::test_scratch::write_config(&path, b"# caf\xe9\n[harness]\nenabled = false\n");
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
            repost: super::Repost::new(),
            disk: 0,
            edits: 0,
            model: aterm_models_harness::harness_policy_one_writer_model(),
        };
        assert_eq!(drive.state(), drive.model.init_state(), "the model's start");

        drive.poll();
        drive.deliver();
        drive.commit();
        drive.poll();
        drive.forget();
        drive.poll();
        drive.admit(true);
        drive.deliver();
        assert!(
            !drive.app.harness_policy.get().policy.enabled,
            "the kill switch runs"
        );
        drive.admit(true);
        assert!(
            !drive.app.harness_policy.get().policy.enabled,
            "it still runs"
        );
        drive.poll();
        drive.write(true);
        drive.admit(false);
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
        drive.write(true);
        drive.admit(true);
        drive.deliver();
        assert!(!drive.app.harness_policy.get().policy.enabled);
        drive.poll();
        drive.deliver();
        assert!(
            drive.app.harness_policy.get().policy.enabled,
            "the newest read runs"
        );

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

    /// A chmod alone must withdraw the policy, without waiting for a content edit.
    #[test]
    #[cfg(unix)]
    fn a_metadata_only_config_refusal_withdraws_the_live_harness_policy() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("aterm.toml");
        let text = "[harness]\nenabled = true\napprove = \"all\"\n";
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let before = config_path_stamp(&path).unwrap();
        let admitted = config_file_observation(&path).unwrap();
        let mut last = None;
        let mut stamps = crate::harness_policy_cell::HarnessStampSource::new();
        let mut posted = Vec::new();
        let policy = observed_harness(Some(&path), &Ok(admitted)).unwrap();
        assert!(policy.policy.enabled);
        assert!(post_harness(
            |policy| {
                posted.push(policy);
                true
            },
            &mut last,
            &mut stamps,
            policy
        ));

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).unwrap();
        let after = config_path_stamp(&path).unwrap();
        assert_ne!(before, after, "permissions alone are a metadata edge");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let (_, refused) = prepare_config_edge_with(Some(&before), after, false, || {
            config_file_observation(&path)
        })
        .unwrap();
        assert!(refused.is_err());
        let off = observed_harness(Some(&path), &refused)
            .expect("an ownership refusal must replace prior harness authority");
        assert!(!off.policy.enabled);
        assert!(post_harness(
            |policy| {
                posted.push(policy);
                true
            },
            &mut last,
            &mut stamps,
            off.clone()
        ));
        assert!(!last.as_ref().unwrap().policy.enabled);
        assert_eq!(posted.len(), 2);
        assert!(post_harness(
            |policy| {
                posted.push(policy);
                true
            },
            &mut last,
            &mut stamps,
            off
        ));
        assert_eq!(posted.len(), 2, "an unchanged refusal does not repost");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let restored = observed_harness(Some(&path), &config_file_observation(&path)).unwrap();
        assert!(restored.policy.enabled);
        assert!(post_harness(
            |policy| {
                posted.push(policy);
                true
            },
            &mut last,
            &mut stamps,
            restored
        ));
        assert_eq!(posted.len(), 3);
    }

    #[test]
    fn a_failed_stamp_replaces_prior_policy_and_missing_context_cannot_enable_it() {
        let mut last = Some(crate::app_config::HarnessPolicy::read(
            "[harness]\nenabled = true\n",
        ));
        let mut stamps = crate::harness_policy_cell::HarnessStampSource::new();
        let mut posted = Vec::new();
        assert!(last.as_ref().unwrap().policy.enabled);
        let path = std::path::Path::new("unavailable/aterm.toml");
        assert!(super::post_stamp_refusal(
            |policy| {
                posted.push(policy);
                true
            },
            &mut last,
            &mut stamps,
            path
        ));
        assert!(!last.as_ref().unwrap().policy.enabled);
        assert_eq!(posted.len(), 1);
        assert!(super::post_stamp_refusal(
            |policy| {
                posted.push(policy);
                true
            },
            &mut last,
            &mut stamps,
            path
        ));
        assert_eq!(posted.len(), 1, "an unchanged failed stamp does not repost");
        for error in [
            crate::native_document_host::DocumentHostError::InvalidUtf8,
            crate::native_document_host::DocumentHostError::TooLarge { limit: 1 },
        ] {
            assert!(!observed_harness(None, &Err(error)).unwrap().policy.enabled);
        }
        assert!(
            crate::app_config::HarnessPolicy::read_path(None)
                .policy
                .enabled,
            "explicit no-config startup retains its defaults"
        );
    }

    /// Accepted document observations keep their own text. Failed file observations use
    /// the common reader, including owned lossy comments, its cap and missing-file defaults.
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
        crate::test_scratch::write_config(&path, text);
        let readable = config_file_observation(&path);
        let policy = observed_harness(Some(&path), &readable).expect("a readable read");
        assert_eq!(
            policy,
            crate::app_config::Config::parse(text).unwrap().harness,
            "the watcher's policy is the one the config's own parse reads"
        );
        assert!(!policy.policy.enabled, "{policy:?}");

        crate::test_scratch::write_config(&path, b"# caf\xe9\n[harness]\nenabled = false\n");
        let refused = config_file_observation(&path);
        assert!(
            matches!(refused, Err(DocumentHostError::InvalidUtf8)),
            "{refused:?}"
        );
        let policy = observed_harness(Some(&path), &refused).expect("a refused read");
        assert!(!policy.policy.enabled, "the kill switch holds: {policy:?}");

        crate::test_scratch::write_config(&path, b"# caf\xe9\n[harness]\napprove = \"all\"\n");
        let refused = config_file_observation(&path);
        let lifted = observed_harness(Some(&path), &refused).expect("a refused read");
        assert!(lifted.policy.enabled, "lifting it holds too: {lifted:?}");

        let big = format!(
            "[harness]\nenabled = false\n# {}\n",
            "x".repeat(crate::native_config_service::MAX_CONFIG_FILE_BYTES)
        );
        crate::test_scratch::write_config(&path, big);
        let refused = config_file_observation(&path);
        assert!(
            matches!(refused, Err(DocumentHostError::TooLarge { .. })),
            "{refused:?}"
        );
        let policy = observed_harness(Some(&path), &refused).expect("a refused read");
        assert!(!policy.policy.enabled, "{policy:?}");

        std::fs::remove_file(&path).unwrap();
        assert!(
            observed_harness(Some(&path), &refused)
                .unwrap()
                .policy
                .enabled,
            "a real missing file uses the absent-file defaults"
        );
        assert!(
            !observed_harness(Some(&path), &Err(DocumentHostError::NotAFile))
                .unwrap()
                .policy
                .enabled,
            "another refusal withdraws prior authority"
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
        crate::test_scratch::write_config(&path, "font_px = 12\n");
        let startup = config_file_observation(&path).unwrap();
        assert!(!initial_config_observation_changed(
            Some(&startup.baseline),
            &startup
        ));

        crate::test_scratch::write_config(&path, "font_px = 14\n");
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
        crate::test_scratch::write_config(&path, b"theme = \"Nord\"\n");
        let metadata = std::fs::metadata(&path).unwrap();
        let modified = metadata.modified().unwrap();
        let accessed = metadata.accessed().unwrap();
        let before = config_file_stamp(&path).unwrap();

        crate::test_scratch::write_config(&path, b"theme = \"Rose\"\n");
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
        crate::test_scratch::write_config(&path, "font_px = 12\n");
        let a = config_file_observation(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let missing = config_file_observation(&path).unwrap();
        crate::test_scratch::write_config(&path, "font_px = 14\n");
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
        crate::test_scratch::write_config(&path, "");
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
        crate::test_scratch::write_config(&first, b"theme = \"Nord\"\n");
        crate::test_scratch::write_config(&second, b"theme = \"Nord\"\n");
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

        after_ctime_tick(&theme);
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
                #[cfg(unix)]
                after_ctime_tick(&theme);
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
        const CAP: usize = 8;
        let caps = crate::app_config::ThemeScanCaps {
            entries: CAP,
            ..crate::app_config::ThemeScanCaps::PRODUCTION
        };
        let dir = std::env::temp_dir().join(format!(
            "aterm-theme-watch-entry-cap-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let noise = (0..=CAP)
            .map(|index| dir.join(format!("noise-{index:04}")))
            .collect::<Vec<_>>();
        for path in &noise {
            std::fs::write(path, []).unwrap();
        }
        let over = theme_directory_stamp_capped(&dir, caps).unwrap();
        assert!(over.truncated);
        assert_eq!(over.inspected_entries, CAP + 1);
        std::fs::remove_file(&noise[0]).unwrap();
        let under = theme_directory_stamp_capped(&dir, caps).unwrap();
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
        let caps = crate::app_config::ThemeScanCaps {
            candidates: 4,
            ..crate::app_config::ThemeScanCaps::PRODUCTION
        };
        let cap = caps.candidates;
        for index in 0..cap + 8 {
            std::fs::write(
                dir.join(format!("Theme-{index:04}.conf")),
                "foreground = #112233\n",
            )
            .unwrap();
        }
        let stamp = theme_directory_stamp_capped(&dir, caps).unwrap();
        assert_eq!(stamp.inspected_entries, cap + 8);
        assert_eq!(stamp.candidate_metadata, cap);

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

        assert!(!schedule.due(start + POLL_INTERVAL - std::time::Duration::from_nanos(1)));
        let first = start + POLL_INTERVAL;
        assert!(schedule.due(first));
        schedule.observed(first, Ok(empty));
        assert!(!schedule.due(first + POLL_INTERVAL));
        let idle_due = first + THEME_IDLE_POLL_INTERVAL;
        assert!(schedule.due(idle_due));

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
        let model = aterm_models_native::watcher_failure_recovery_model();
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
        crate::test_scratch::write_config(&path, [0xff, 0xfe]);
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
        crate::test_scratch::write_config(&path, "font_px = 12\n");
        let first = config_file_observation(&path).unwrap().baseline;
        crate::test_scratch::write_config(&path, "font_px = 14\n");
        let second = config_file_observation(&path).unwrap().baseline;
        assert_ne!(first, second);

        let mut state = WatchStatusState::default();
        let model = aterm_models_native::watcher_failure_recovery_model();
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

    /// Rewrite the config at `path` as an editor would, once the filesystem's clock has moved
    /// past its last change: two same-length writes inside one tick leave every stamp equal.
    fn rewrite(path: &std::path::Path, text: impl AsRef<[u8]>) {
        #[cfg(unix)]
        after_ctime_tick(path);
        crate::test_scratch::write_config(path, text);
    }

    /// The real watcher loop ([`super::watch`]) over scripted ears: a clock that moves only when
    /// a rest runs out, every key held and let go (a key is never used twice), each rest's
    /// deadline from the clock, and the script's turns, one at each rest. A turn may change
    /// files and returns what it tells; one that tells nothing lets the rest run out. The watch
    /// ends at the script's end.
    struct Script {
        now: std::time::Instant,
        deaf: bool,
        held: std::collections::BTreeMap<u64, super::Hold>,
        keys: std::collections::BTreeSet<u64>,
        let_go: Vec<u64>,
        rests: Vec<Option<std::time::Duration>>,
        turns: std::collections::VecDeque<Turn>,
    }

    type Turn = Box<dyn FnOnce(&mut Script) -> Vec<super::Tell>>;

    impl Script {
        fn new(deaf: bool, turns: Vec<Turn>) -> Self {
            Self {
                now: std::time::Instant::now(),
                deaf,
                held: std::collections::BTreeMap::new(),
                keys: std::collections::BTreeSet::new(),
                let_go: Vec::new(),
                rests: Vec::new(),
                turns: turns.into(),
            }
        }

        /// A tell of each key held on `path`.
        fn on(&self, path: &std::path::Path) -> Vec<super::Tell> {
            let tells: Vec<_> = self
                .held
                .iter()
                .filter(|(_, hold)| {
                    matches!(hold, super::Hold::File(held) | super::Hold::Directory(held) if held == path)
                })
                .map(|(key, _)| super::Tell::Changed(*key))
                .collect();
            assert!(!tells.is_empty(), "{} is held", path.display());
            tells
        }

        fn rest(&self) -> Option<std::time::Duration> {
            *self.rests.last().expect("a rest")
        }

        fn holds(&self) -> std::collections::BTreeSet<super::Hold> {
            self.held.values().cloned().collect()
        }
    }

    impl super::Ears for Script {
        fn watch(&mut self, key: u64, path: std::path::PathBuf) {
            assert!(self.keys.insert(key), "key {key} used again");
            self.held.insert(key, super::Hold::File(path));
        }

        fn watch_directory(&mut self, key: u64, path: std::path::PathBuf) {
            assert!(self.keys.insert(key), "key {key} used again");
            self.held.insert(key, super::Hold::Directory(path));
        }

        fn unwatch(&mut self, key: u64) {
            assert!(self.held.remove(&key).is_some(), "key {key} let go twice");
            self.let_go.push(key);
        }

        fn deaf(&self) -> bool {
            self.deaf
        }

        fn now(&self) -> std::time::Instant {
            self.now
        }

        fn wait(
            &mut self,
            until: Option<std::time::Instant>,
            tells: &mut Vec<super::Tell>,
        ) -> Option<std::time::Instant> {
            self.rests
                .push(until.map(|until| until.saturating_duration_since(self.now)));
            let turn = self.turns.pop_front()?;
            let told = turn(self);
            if told.is_empty() {
                self.now = until.expect("a rest no clock ends and nothing tells never ends");
            }
            tells.extend(told);
            Some(self.now)
        }
    }

    /// What the watcher posted, in order.
    #[derive(Clone, Default)]
    struct Posts(std::rc::Rc<std::cell::RefCell<Vec<crate::Wake>>>);

    impl Posts {
        /// The text of each config read posted.
        fn reads(&self) -> Vec<String> {
            self.0
                .borrow()
                .iter()
                .filter_map(|wake| match wake {
                    crate::Wake::ConfigReloadObserved(observation) => {
                        Some(observation.text.clone())
                    }
                    _ => None,
                })
                .collect()
        }

        fn statuses(&self) -> Vec<WatchStatusEvent> {
            self.0
                .borrow()
                .iter()
                .filter_map(|wake| match wake {
                    crate::Wake::ConfigWatchStatus(event) => Some(*event),
                    _ => None,
                })
                .collect()
        }

        fn policies(&self) -> usize {
            self.0
                .borrow()
                .iter()
                .filter(|wake| matches!(wake, crate::Wake::HarnessPolicyObserved(_)))
                .count()
        }

        fn catalogs(&self) -> usize {
            self.0
                .borrow()
                .iter()
                .filter(|wake| matches!(wake, crate::Wake::ThemeCatalogChanged(_)))
                .count()
        }
    }

    /// Run the real watcher on `config` and `themes` over `script`, every post into `posts`.
    fn run_watch(
        config: Option<&std::path::Path>,
        themes: Option<&std::path::Path>,
        script: &mut Script,
        posts: &Posts,
        repost: &super::Repost,
    ) {
        let initial = themes.map_or_else(
            || std::sync::Arc::new(crate::app_config::ThemeCatalog::default()),
            |dir| {
                std::sync::Arc::new(crate::app_config::ThemeCatalog::try_discover_in(dir).unwrap())
            },
        );
        let sink = posts.clone();
        super::watch(
            config.map(std::path::Path::to_path_buf),
            None,
            initial,
            themes.map(std::path::Path::to_path_buf),
            &mut move |wake| {
                sink.0.borrow_mut().push(wake);
                true
            },
            script,
            repost,
        );
    }

    fn watch_scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("aterm-config-watch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(&dir).unwrap()
    }

    /// At rest the watcher waits for a tell with no clock. A tell whose stamps did not move reads
    /// nothing; a tell of an edit reads and posts it; a theme file's bytes rewritten in place,
    /// which changes no entry of its folder, are held and told too.
    #[test]
    fn the_watcher_rests_untimed_and_reads_what_it_is_told() {
        let dir = watch_scratch("rest");
        let config = dir.join("aterm.toml");
        crate::test_scratch::write_config(&config, "font_px = 14\n");
        let themes = dir.join("themes");
        std::fs::create_dir(&themes).unwrap();
        let theme = themes.join("paper.conf");
        std::fs::write(&theme, "foreground = #202020\nbackground = #fafafa\n").unwrap();
        let posts = Posts::default();
        let seen = posts.clone();
        let (edited, conf, file) = (config.clone(), themes.clone(), theme.clone());
        let mut script = Script::new(
            false,
            vec![
                // The first theme step's holds: told at their first look, as a file watch does.
                Box::new(move |s: &mut Script| {
                    let mut tells = s.on(&conf);
                    tells.extend(s.on(&file));
                    tells
                }),
                Box::new(move |s: &mut Script| {
                    assert_eq!(s.rest(), None, "nothing to look at on a clock");
                    assert_eq!(seen.reads().len(), 1, "the first read");
                    s.on(&edited)
                }),
                Box::new({
                    let seen = posts.clone();
                    let edited = config.clone();
                    move |s: &mut Script| {
                        assert_eq!(
                            seen.reads().len(),
                            1,
                            "a tell that moved no stamp reads nothing"
                        );
                        rewrite(&edited, "font_px = 16\n");
                        s.on(&edited)
                    }
                }),
                Box::new({
                    let seen = posts.clone();
                    let file = theme.clone();
                    move |s: &mut Script| {
                        assert_eq!(
                            seen.reads().last().map(String::as_str),
                            Some("font_px = 16\n")
                        );
                        assert_eq!(s.rest(), None);
                        #[cfg(unix)]
                        after_ctime_tick(&file);
                        std::fs::write(&file, "foreground = #101010\nbackground = #ffffff\n")
                            .unwrap();
                        s.on(&file)
                    }
                }),
            ],
        );
        run_watch(
            Some(&config),
            Some(&themes),
            &mut script,
            &posts,
            &super::Repost::new(),
        );
        assert_eq!(posts.reads().len(), 2);
        assert_eq!(posts.catalogs(), 1, "the theme file's bytes, told");
        assert_eq!(script.rest(), None);
        assert_eq!(
            script.holds(),
            [
                super::Hold::File(config.clone()),
                super::Hold::File(theme.clone()),
                super::Hold::Directory(themes.clone()),
            ]
            .into_iter()
            .collect(),
            "the config, the theme folder and the theme file it read"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A config reached through links holds each one and the file they end at, in another folder:
    /// that file edited in place, which changes nothing the link's folder holds, is read.
    #[cfg(unix)]
    #[test]
    fn a_linked_config_holds_each_hop_and_reads_the_file_it_names_edited() {
        let dir = watch_scratch("links");
        let dotfiles = dir.join("dotfiles");
        let conf = dir.join("conf");
        std::fs::create_dir(&dotfiles).unwrap();
        std::fs::create_dir(&conf).unwrap();
        let target = dotfiles.join("aterm.toml");
        crate::test_scratch::write_config(&target, "font_px = 14\n");
        let middle = conf.join("middle.toml");
        std::os::unix::fs::symlink(&target, &middle).unwrap();
        let config = conf.join("aterm.toml");
        std::os::unix::fs::symlink("middle.toml", &config).unwrap();
        let posts = Posts::default();
        let edited = target.clone();
        let mut script = Script::new(
            false,
            vec![Box::new(move |s: &mut Script| {
                rewrite(&edited, "font_px = 18\n");
                s.on(&edited)
            })],
        );
        run_watch(
            Some(&config),
            None,
            &mut script,
            &posts,
            &super::Repost::new(),
        );
        assert_eq!(
            script.holds(),
            [config.clone(), middle.clone(), target.clone()]
                .into_iter()
                .map(super::Hold::File)
                .collect(),
            "every hop"
        );
        assert_eq!(
            posts.reads().last().map(String::as_str),
            Some("font_px = 18\n")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A refusal no edit ends (a file its user cannot read) is looked at again on a clock that
    /// backs off to its most; once readable, the config is read and the watcher rests untimed.
    #[cfg(unix)]
    #[test]
    fn a_refusal_no_edit_ends_is_looked_at_again_backing_off() {
        use std::os::unix::fs::PermissionsExt as _;
        aterm_uds::host::could_not_run_as_root("a config of mode 000");
        let dir = watch_scratch("refused");
        let config = dir.join("aterm.toml");
        crate::test_scratch::write_config(&config, "font_px = 14\n");
        let posts = Posts::default();
        let mode = |path: &std::path::Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        let mut turns: Vec<Turn> = vec![Box::new({
            let config = config.clone();
            move |s: &mut Script| {
                mode(&config, 0o000);
                s.on(&config)
            }
        })];
        for _ in 0..7 {
            turns.push(Box::new(|_: &mut Script| Vec::new()));
        }
        turns.push(Box::new({
            let config = config.clone();
            move |_: &mut Script| {
                mode(&config, 0o600);
                Vec::new()
            }
        }));
        let mut script = Script::new(false, turns);
        run_watch(
            Some(&config),
            None,
            &mut script,
            &posts,
            &super::Repost::new(),
        );
        let ms = |ms| Some(std::time::Duration::from_millis(ms));
        assert_eq!(
            script.rests,
            [
                None,
                ms(500),
                ms(1000),
                ms(2000),
                ms(4000),
                ms(8000),
                ms(10_000),
                ms(10_000),
                ms(10_000),
                None,
            ],
            "no clock, then one that backs off from the refusal, then none again"
        );
        assert!(
            posts
                .statuses()
                .contains(&WatchStatusEvent::Failed(WatchFailure {
                    target: WatchTarget::Config,
                    kind: WatchFailureKind::ConfigUnreadable,
                }))
        );
        assert_eq!(
            posts.reads().len(),
            2,
            "read at start and once readable again"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A failure an edit ends (bytes that are not UTF-8) sets no clock: the edit that ends it is
    /// told, and read.
    #[test]
    fn a_failure_an_edit_ends_waits_for_the_edit() {
        let dir = watch_scratch("edit-ends");
        let config = dir.join("aterm.toml");
        crate::test_scratch::write_config(&config, "font_px = 14\n");
        let posts = Posts::default();
        let mut script = Script::new(
            false,
            vec![
                Box::new({
                    let config = config.clone();
                    move |s: &mut Script| {
                        rewrite(&config, b"# caf\xe9\n");
                        s.on(&config)
                    }
                }),
                Box::new({
                    let config = config.clone();
                    move |s: &mut Script| {
                        assert_eq!(s.rest(), None, "no clock for a failure an edit ends");
                        rewrite(&config, "font_px = 15\n");
                        s.on(&config)
                    }
                }),
            ],
        );
        run_watch(
            Some(&config),
            None,
            &mut script,
            &posts,
            &super::Repost::new(),
        );
        assert!(
            posts
                .statuses()
                .contains(&WatchStatusEvent::Failed(WatchFailure {
                    target: WatchTarget::Config,
                    kind: WatchFailureKind::ConfigInvalidUtf8,
                }))
        );
        assert_eq!(
            posts.reads().last().map(String::as_str),
            Some("font_px = 15\n")
        );
        assert_eq!(script.rest(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A seamless successor's Commit wakes the resting watcher (its [`super::Repost`] tells the
    /// ring it listens on), which posts the policy, the config and the theme catalog again, as
    /// they stand: the window dropped every wake before it.
    #[test]
    fn a_commit_wakes_the_watcher_to_post_everything_again() {
        let dir = watch_scratch("repost");
        let config = dir.join("aterm.toml");
        crate::test_scratch::write_config(&config, "font_px = 14\n");
        let themes = dir.join("themes");
        std::fs::create_dir(&themes).unwrap();
        let posts = Posts::default();
        let repost = std::sync::Arc::new(super::Repost::new());
        let (ring, heard) = std::sync::mpsc::channel();
        repost.listen(ring);
        let raised = std::sync::Arc::clone(&repost);
        let seen = posts.clone();
        let mut script = Script::new(
            false,
            vec![Box::new(move |_: &mut Script| {
                assert_eq!(
                    (seen.policies(), seen.reads().len(), seen.catalogs()),
                    (1, 1, 0),
                    "control: one of each read at the start, and the catalog as it stood"
                );
                raised.raise();
                heard.try_iter().collect()
            })],
        );
        run_watch(Some(&config), Some(&themes), &mut script, &posts, &repost);
        assert_eq!(
            (posts.policies(), posts.reads().len(), posts.catalogs()),
            (2, 2, 1),
            "each posted again"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The file watch's thread ending turns the watcher to its clock: it looks every
    /// [`POLL_INTERVAL`], so an edit no event tells is read at the next look.
    #[test]
    fn events_ended_turns_the_watcher_to_its_clock() {
        let dir = watch_scratch("ended");
        let config = dir.join("aterm.toml");
        crate::test_scratch::write_config(&config, "font_px = 14\n");
        let posts = Posts::default();
        let mut script = Script::new(
            false,
            vec![
                Box::new(|_: &mut Script| vec![super::Tell::EventsEnded]),
                Box::new({
                    let config = config.clone();
                    move |s: &mut Script| {
                        assert_eq!(s.rest(), Some(POLL_INTERVAL));
                        rewrite(&config, "font_px = 17\n");
                        Vec::new()
                    }
                }),
            ],
        );
        run_watch(
            Some(&config),
            None,
            &mut script,
            &posts,
            &super::Repost::new(),
        );
        assert_eq!(
            posts.reads().last().map(String::as_str),
            Some("font_px = 17\n")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A watcher that hears nothing looks on today's clock: the config every [`POLL_INTERVAL`]
    /// from the start, and an edit is read at the next look.
    #[test]
    fn deaf_ears_look_every_poll_interval() {
        let dir = watch_scratch("deaf");
        let config = dir.join("aterm.toml");
        crate::test_scratch::write_config(&config, "font_px = 14\n");
        let posts = Posts::default();
        let mut script = Script::new(
            true,
            vec![
                Box::new({
                    let config = config.clone();
                    move |s: &mut Script| {
                        assert_eq!(s.rest(), Some(POLL_INTERVAL));
                        rewrite(&config, "font_px = 19\n");
                        Vec::new()
                    }
                }),
                Box::new(|s: &mut Script| {
                    assert_eq!(s.rest(), Some(POLL_INTERVAL));
                    Vec::new()
                }),
            ],
        );
        run_watch(
            Some(&config),
            None,
            &mut script,
            &posts,
            &super::Repost::new(),
        );
        assert_eq!(
            posts.reads().last().map(String::as_str),
            Some("font_px = 19\n")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A key's hold changes only by what left its step's set and what came: a path still wanted
    /// keeps its key, one let go is unwatched, and a path that comes again gets a new key, so a
    /// tell queued for the old one cannot be taken for it.
    #[test]
    fn held_keys_change_by_the_difference_and_are_never_reused() {
        use super::{Ears as _, Hold, Owes};
        let file = |name: &str| Hold::File(std::path::PathBuf::from(format!("/scratch/{name}")));
        let mut ears = Script::new(false, Vec::new());
        let mut held = super::Held::default();
        held.hold(&mut ears, Owes::Config, [file("a"), file("b")].into());
        held.hold(&mut ears, Owes::Themes, [file("a")].into());
        let first = ears.held.clone();
        held.hold(&mut ears, Owes::Config, [file("b"), file("c")].into());
        assert_eq!(ears.let_go, [0], "a let go");
        assert_eq!(ears.held.get(&1), first.get(&1), "b kept its key");
        assert_eq!(
            ears.held.get(&2),
            Some(&file("a")),
            "the theme step's own a"
        );
        assert_eq!(held.owes(2), Some(Owes::Themes));
        held.hold(
            &mut ears,
            Owes::Config,
            [file("a"), file("b"), file("c")].into(),
        );
        assert_eq!(held.owes(0), None, "the old key is no one's");
        assert_eq!(
            held.owes(4),
            Some(Owes::Config),
            "a came again with a new key"
        );
        assert!(!ears.deaf());
    }

    /// The retry clock: none for a success or a failure an edit ends, and from
    /// [`POLL_INTERVAL`] doubling to its most while the failure no edit ends goes on.
    #[test]
    fn the_retry_clock_backs_off_only_for_a_failure_no_edit_ends() {
        let start = std::time::Instant::now();
        let mut retry = super::Retry::default();
        retry.after(start, Some(WatchFailureKind::ConfigInvalidUtf8));
        assert_eq!(retry.at, None);
        let mut waits = Vec::new();
        let mut now = start;
        for _ in 0..7 {
            retry.after(now, Some(WatchFailureKind::ConfigUnreadable));
            let at = retry.at.unwrap();
            waits.push(at - now);
            assert!(!retry.due(at - std::time::Duration::from_nanos(1)) && retry.due(at));
            now = at;
        }
        let s = std::time::Duration::from_millis;
        assert_eq!(
            waits,
            [
                s(500),
                s(1000),
                s(2000),
                s(4000),
                s(8000),
                super::RETRY_MOST,
                super::RETRY_MOST
            ]
        );
        retry.after(now, None);
        assert_eq!(retry.at, None, "a success");
        retry.after(now, Some(WatchFailureKind::ThemeEntryUnreadable));
        assert_eq!(
            retry.at,
            Some(now + POLL_INTERVAL),
            "a new failure starts again"
        );
    }

    /// The watcher over this platform's file events reads an edit it hears, and ends once the
    /// window refuses its posts.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_platform_watcher_reads_an_edit_it_hears_and_ends_with_the_window() {
        let dir = watch_scratch("platform");
        let config = dir.join("aterm.toml");
        crate::test_scratch::write_config(&config, "font_px = 14\n");
        let (posted, posts) = std::sync::mpsc::channel::<String>();
        let path = config.clone();
        let watcher = std::thread::spawn(move || {
            let (ring, tells) = std::sync::mpsc::channel();
            let repost = super::Repost::new();
            repost.listen(ring.clone());
            let mut ears = super::platform_ears(ring, tells);
            assert!(!ears.deaf(), "this platform's file events started");
            super::watch(
                Some(path),
                None,
                std::sync::Arc::new(crate::app_config::ThemeCatalog::default()),
                None,
                &mut move |wake| match wake {
                    crate::Wake::ConfigReloadObserved(observation) => {
                        posted.send(observation.text).is_ok()
                    }
                    _ => true,
                },
                ears.as_mut(),
                &repost,
            );
        });
        let must = aterm_spec::wait::MUST_HAPPEN;
        assert_eq!(
            posts.recv_timeout(must).as_deref(),
            Ok("font_px = 14\n"),
            "the first read"
        );
        rewrite(&config, "font_px = 16\n");
        assert_eq!(
            posts.recv_timeout(must).as_deref(),
            Ok("font_px = 16\n"),
            "the edit, heard"
        );
        drop(posts);
        rewrite(&config, "font_px = 18\n");
        aterm_spec::wait::until_within(must, "the watcher ends with its posts refused", || {
            watcher.is_finished().then_some(())
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A theme folder of more files than discovery reads holds a key on each file it reads, in
    /// name order, and on none it never reads: past its count, or a name it does not take.
    #[test]
    fn a_theme_folder_holds_only_the_files_discovery_reads() {
        let dir = watch_scratch("theme-cap");
        let themes = dir.join("themes");
        std::fs::create_dir(&themes).unwrap();
        let most = crate::app_config::MAX_USER_THEME_FILES;
        let shadowed = aterm_types::scheme::builtin_names()[0].to_owned();
        for name in (0..most + 2)
            .map(|n| format!("t{n:04}"))
            .chain([shadowed.clone()])
        {
            std::fs::write(
                themes.join(format!("{name}.conf")),
                "foreground = #202020\nbackground = #fafafa\n",
            )
            .unwrap();
        }
        let posts = Posts::default();
        // The first rest, with no turn, ends the run once the first theme step held its keys.
        let mut script = Script::new(false, Vec::new());
        run_watch(
            None,
            Some(&themes),
            &mut script,
            &posts,
            &super::Repost::new(),
        );
        let files: Vec<_> = script
            .holds()
            .into_iter()
            .filter_map(|hold| match hold {
                super::Hold::File(path) => Some(path),
                super::Hold::Directory(_) => None,
            })
            .collect();
        let expected: Vec<_> = (0..most)
            .map(|n| themes.join(format!("t{n:04}.conf")))
            .collect();
        assert_eq!(
            files, expected,
            "the first {most} it reads, and not {shadowed}.conf"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
