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
                    "the window's settings except [harness] and [update]'s two switches are at \
                     their defaults"
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
        not(any(target_os = "macos", target_os = "linux", windows, test)),
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
        not(any(target_os = "macos", target_os = "linux", windows, test)),
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
/// start, or off macOS, Linux and Windows, a clock alone ([`Deaf`]).
fn platform_ears(ring: mpsc::Sender<Tell>, tells: mpsc::Receiver<Tell>) -> Box<dyn Ears> {
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
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
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    drop(ring);
    Box::new(Deaf { tells })
}

/// Owned by the file watch's notify: dropped as that watch's thread ends, it tells the watcher
/// so ([`Tell::EventsEnded`]).
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
struct Ended(mpsc::Sender<Tell>);

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
impl Drop for Ended {
    fn drop(&mut self) {
        let _ = self.0.send(Tell::EventsEnded);
    }
}

/// The platform's file events ([`aterm_uds::file_watch::FileWatch`]). On Windows the watch
/// listens only inside the account's application-data and temporary folders, where the config and
/// the themes are by default; a config, a feed or a link's target anywhere else (a project) is
/// looked at on the watch's own clock, with nothing held open that would keep its folders from
/// being renamed, and is told the same way.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
struct Heard {
    watch: aterm_uds::file_watch::FileWatch,
    tells: mpsc::Receiver<Tell>,
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
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

/// No file events (off macOS, Linux and Windows, or where they did not start): the config is
/// looked at every [`POLL_INTERVAL`] and the themes on their [`ThemePollSchedule`], and only a
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

/// When a step whose last failure can end with no event ([`ends_with_no_edit`]) looks again:
/// [`POLL_INTERVAL`] after the step ended, each time it fails so, as the stat clock before the
/// events looked. A success, or a failure an edit ends, sets no clock.
#[derive(Default)]
struct Retry {
    at: Option<Instant>,
}

impl Retry {
    fn due(&self, now: Instant) -> bool {
        self.at.is_some_and(|at| now >= at)
    }

    fn after(&mut self, ended: Instant, failure: Option<WatchFailureKind>) {
        self.at = failure
            .is_some_and(ends_with_no_edit)
            .then(|| ended + POLL_INTERVAL);
    }

    /// Look again [`POLL_INTERVAL`] after the step ended, whatever the last failure was: a refused
    /// read waits on the next one to agree with it ([`post_refused_read`]), and a refusal an edit
    /// ends sets no clock of its own.
    fn confirm(&mut self, ended: Instant) {
        self.at = Some(ended + POLL_INTERVAL);
    }
}

/// Whether a failure of `kind` can end with no event the watch hears, so the step looks again on
/// a clock: a refusal or an I/O error (a privacy grant, or a `chmod` macOS tells no folder of,
/// lifts one unseen), and a theme scan that changed as it read, whose stamp covers files no key
/// holds (one past the catalog's cap, or one a built-in shadows), written in place where only
/// their folder listens. An edit ends the rest, and its event wakes the step: bytes that do not
/// read, a file too large or not a file, a binding through an unsafe link, or a change made as
/// the config, every file of which a key holds, read.
const fn ends_with_no_edit(kind: WatchFailureKind) -> bool {
    match kind {
        WatchFailureKind::ConfigUnreadable
        | WatchFailureKind::ThemeDirectoryUnreadable
        | WatchFailureKind::ThemeEntryUnreadable
        | WatchFailureKind::ThemeFileUnavailable
        | WatchFailureKind::ThemeChangedWhileScanning => true,
        WatchFailureKind::ConfigInvalidUtf8
        | WatchFailureKind::ConfigInvalidToml
        | WatchFailureKind::ConfigTooLarge
        | WatchFailureKind::ConfigNotRegular
        | WatchFailureKind::ConfigChangedWhileReading
        | WatchFailureKind::ConfigUnsafeBinding
        | WatchFailureKind::ConfigPreparationFailed => false,
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
    let mut refused_reads = RefusedReads::default();
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
    // When the last step that looked ended, until the rest after it.
    let mut looked: Option<Instant> = None;
    loop {
        if forget_after_commit(repost, &mut last_harness) {
            refused_reads.sent = None;
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
                    if !post_refused_update_text(&mut refused_reads, path, |text| {
                        post(Wake::RefusedUpdateText(text))
                    }) {
                        return;
                    }
                }
                Ok(stamp) => {
                    let feeds_moved = path_feed_stamp(&feed_paths) != last_feed_stamp;
                    let candidate = prepare_config_edge_with(
                        last_config_stamp.as_ref(),
                        stamp,
                        config_needs_retry || feeds_moved,
                        || {
                            crate::native_config_service::observe_through_cursor_migration(
                                &crate::native_config_service::CursorMigrationStamp::for_config(
                                    path,
                                ),
                                || config_file_observation(path),
                            )
                        },
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
                                // An admitted read replaces what the window holds, so a refusal
                                // after it sends its text even if that text went before.
                                refused_reads = RefusedReads::default();
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
                                if !post_refused_update_text(&mut refused_reads, path, |text| {
                                    post(Wake::RefusedUpdateText(text))
                                }) {
                                    return;
                                }
                            }
                        }
                    }
                }
            }
            let ended = ears.now();
            looked = Some(ended);
            config_retry.after(ended, failed);
            if refused_reads.awaits_agreement() {
                config_retry.confirm(ended);
            }
            if let Some(clock) = deaf.as_mut() {
                clock.config_at = ended + POLL_INTERVAL;
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
            let ended = ears.now();
            looked = Some(ended);
            theme_retry.after(ended, sample.as_ref().err().copied());
            if let Some(clock) = deaf.as_mut() {
                clock.themes.observed(ended, sample);
            }
            held.hold(
                ears,
                Owes::Themes,
                theme_holds(directory, &theme_candidates),
            );
        }

        // Each clock counts from its own step's end, and none comes sooner than POLL_INTERVAL
        // after the last look: a look slower than its interval (a share answering slowly) still
        // rests between looks, as the stat clock before the events slept after each.
        let last = looked.take();
        let until = earliest([
            config_retry.at,
            theme_retry.at,
            deaf.as_ref()
                .filter(|_| path.is_some())
                .map(|clock| clock.config_at),
            deaf.as_ref()
                .filter(|_| theme_dir.is_some())
                .map(|clock| clock.themes.next_poll),
        ])
        .map(|at| last.map_or(at, |last| at.max(last + POLL_INTERVAL)));
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

/// What [`post_refused_update_text`] last read and last sent.
#[derive(Debug, Default)]
struct RefusedReads {
    read: Option<String>,
    sent: Option<String>,
}

impl RefusedReads {
    /// Whether the last read's text is neither sent nor yet agreed by a second read.
    fn awaits_agreement(&self) -> bool {
        self.read.is_some() && self.read != self.sent
    }
}

/// A read this watcher's admission refused: send what the file holds, as [`post_refused_read`]
/// decides. What it may do to the window's update switches is the window's to judge
/// (`App::take_refused_update_text`). A seamless successor drops every config wake before its
/// Commit, so the Commit forgets what was sent and the next agreeing read sends it again. Returns
/// whether the window is still there.
fn post_refused_update_text(
    reads: &mut RefusedReads,
    path: &std::path::Path,
    send: impl FnOnce(String) -> bool,
) -> bool {
    post_refused_read(
        reads,
        aterm_update_core::settings::read_config_text(path),
        send,
    )
}

/// Send the text of this read ([`crate::app_config::refused_read_text`]) once two reads in a row agree
/// on it and it is not what was last sent. A refusal is a failure no edit ends, so the watcher reads
/// the file again on its [`Retry`] clock and the second read is due within [`POLL_INTERVAL`]. A read
/// that caught a save half way therefore moves nothing unless that save stalled across two reads,
/// and a fault that can pass starts the agreement again.
fn post_refused_read(
    reads: &mut RefusedReads,
    read: std::io::Result<Option<String>>,
    send: impl FnOnce(String) -> bool,
) -> bool {
    let Some(text) = crate::app_config::refused_read_text(read) else {
        reads.read = None;
        return true;
    };
    let agreed = reads.read.replace(text.clone()).as_ref() == Some(&text);
    if !agreed || reads.sent.as_ref() == Some(&text) {
        return true;
    }
    reads.sent = Some(text.clone());
    send(text)
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
#[path = "config_watcher_tests.rs"]
pub(crate) mod tests;
