// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lab model's local upkeep (docs/DESIGN-alab-lab-model-2026-11-01.md §11 step 8), run by
//! the Commons owner (`crate::runtime`, the window's or `aterm node serve`'s, whichever holds
//! the owner lease) beside its repair rounds: the journal's flusher (`super::journal::flush`),
//! on its own `alab-journal` thread so the repair loop never waits on a table read. A pass a
//! minute (ten minutes after one that failed or was kept local), each a rename of the spool
//! and, only when a flush may be due, a read of the week tables. No network read of any other
//! service, and no provider is asked anything. Self-registration (step 7) is
//! `super::register`'s, driven by the `aterm` executable from the same repair rounds.
//!
//! The shared files' materializer (docs/DESIGN-shared-files-2026-11-11.md §4) is the owner's
//! too, on its own `alab-files` thread ([`Materializer`]): a pass shortly after a table store of
//! this process grew (a record it authored or took from a peer), and one a minute besides, for
//! what another process of this Mac appended, for what changes without a record of the table
//! (a membership record that makes an owned row readable) and for what custody refuses now
//! (`[commons]` off, membership, a succession), none of which a store write of this process
//! signals. Between them the thread sleeps until one is due, woken by the growth itself
//! (`crate::file::growth`) and by nothing else but its stop and the owner's moves. Each pass
//! projects only the files whose projection key moved, and writes only what changed, so a pass at
//! rest writes nothing. It passes whether or not the owner is serving, and each pass writes only
//! under `~/.aterm/files`. The same thread runs the shared files' publisher before each pass
//! (`crate::file::publish`): the writes this Mac queued are signed there, one record each.
//!
//! The registry's secret store is swept on an `alab-secrets` thread ([`Secrets`]), hourly: what
//! an ended run left owed is removed from the vault once no cell names it
//! (`super::secret::housekeep`). A pass with nothing owed reads one directory of this Mac's.

use super::journal::{Presence, Spool, Word, flush::Flushed};
use crate::file::growth::GROWTH;
use aterm_secrets::client::Paths;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// The first pass waits this long after the owner starts serving, so its first repair rounds run
/// first.
pub(crate) const FIRST: Duration = Duration::from_secs(60);
/// A pass that failed or kept its record local is tried again this much later.
pub(crate) const RETRY: Duration = Duration::from_secs(10 * 60);

/// A journal pass at most this often.
pub(crate) const JOURNAL_EVERY: Duration = Duration::from_secs(60);

/// The journal's spool this build flushes, and the one the shared files' publisher notes what it
/// published in (`crate::file::publish::Pass::journal`): this user's (`Spool::local`), or none
/// in this crate's tests, whose runtime labs must not touch the host's state.
pub(crate) fn journal_spool() -> Option<Spool> {
    if cfg!(test) { None } else { Spool::local() }
}

/// The journal's flusher on its own thread: a pass every [`JOURNAL_EVERY`], [`RETRY`] after one
/// that failed or kept its record local, and at once after an update record's
/// [`super::journal::upd::nudge`].
struct Journal {
    spool: Spool,
    next: Instant,
    running: Option<JoinHandle<Option<Flushed>>>,
}

impl Journal {
    fn tick(&mut self, paths: &Paths, now: Instant) {
        if self.running.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(handle) = self.running.take()
        {
            let flushed = handle.join().ok().flatten();
            self.next = now
                + match flushed {
                    Some(Flushed::Idle | Flushed::Wrote { .. }) => JOURNAL_EVERY,
                    Some(Flushed::Kept(_)) | None => RETRY,
                };
        }
        if self.running.is_none() && super::journal::upd::nudged() {
            self.next = now;
        }
        if self.running.is_some() || now < self.next {
            return;
        }
        let (paths, spool) = (paths.clone(), self.spool.clone());
        self.running = std::thread::Builder::new()
            .name("alab-journal".into())
            .spawn(move || {
                let now = aterm_time::unix_millis()? / 1000;
                if now == 0 {
                    return None;
                }
                // Read each pass, so `[alab] journal = false` takes hold within a minute.
                let off =
                    !super::journal::publishing(aterm_types::dirs::aterm_config_path().as_deref());
                super::journal::flush::pass_observed(
                    &paths,
                    &spool,
                    &|| presence_live(&paths),
                    &super::journal::place::holdings,
                    super::journal::mine::logins(),
                    &super::journal::upd::record,
                    off,
                    now,
                )
                .ok()
            })
            .ok();
        if self.running.is_none() {
            self.next = now + RETRY;
        }
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        if let Some(running) = self.running.take()
            && running.thread().id() != std::thread::current().id()
        {
            let _ = running.join();
        }
    }
}

/// When the shared files' materializer passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cadence {
    /// The first pass waits this long after the owner starts, so its first try at serving
    /// runs first.
    pub(crate) first: Duration,
    /// A pass waits this long after a store grew, so a burst of records is one pass.
    pub(crate) settle: Duration,
    /// A pass at least this often, whether or not this process saw a store grow.
    pub(crate) every: Duration,
    /// A pass that met a busy vault or store is followed by another this much later: another
    /// program's hold is brief, and its word should not stand for a minute.
    pub(crate) busy: Duration,
}

impl Cadence {
    /// The cadence of a running aterm.
    pub(crate) const LIVE: Self = Self {
        first: Duration::from_secs(5),
        settle: Duration::from_secs(2),
        every: Duration::from_secs(60),
        busy: Duration::from_secs(10),
    };
}

/// When the shared files' materializer is due: a pass shortly after a table store of this
/// process grew, and one each [`Cadence::every`] besides.
struct Files {
    cadence: Cadence,
    /// The growth count the last pass started at.
    seen: u64,
    /// When a wake first saw the count past `seen`.
    moved_at: Option<Instant>,
    /// When the next pass is due whether or not a store grew.
    regular: Instant,
}

impl Files {
    /// The schedule of an owner that starts at `now`, the growth count reading `growth`: its first
    /// pass is due [`Cadence::first`] later.
    fn new(cadence: Cadence, now: Instant, growth: u64) -> Self {
        Self {
            cadence,
            seen: growth,
            moved_at: None,
            regular: now + cadence.first,
        }
    }

    /// When the next pass is due: [`Cadence::settle`] after a wake saw a store grow, and never
    /// later than the regular one.
    fn next_at(&self) -> Instant {
        self.moved_at.map_or(self.regular, |moved| {
            (moved + self.cadence.settle).min(self.regular)
        })
    }

    /// The growth count the wait for the next pass wakes once the count leaves it: the one the
    /// last pass started at, until a wake sees it moved. Inside the settle no growth wakes the
    /// wait (`None`), so a burst is one wake and one pass, and its appends ring nothing.
    fn awaits(&self) -> Option<u64> {
        self.moved_at.is_none().then_some(self.seen)
    }

    /// Whether the growth count reading `growth` wakes the wait for the next pass
    /// ([`Self::awaits`]).
    fn wakes(&self, growth: u64) -> bool {
        self.awaits().is_some_and(|seen| growth != seen)
    }

    /// Whether a pass is due at `now`, the growth count reading `growth` ([`Self::next_at`], once
    /// a growth that wakes the wait is noted). A pass that is due starts the schedule again at
    /// `now`.
    fn due(&mut self, now: Instant, growth: u64) -> bool {
        if self.wakes(growth) {
            self.moved_at = Some(now);
        }
        if now < self.next_at() {
            return false;
        }
        self.seen = growth;
        self.moved_at = None;
        self.regular = now + self.cadence.every;
        true
    }

    /// A pass that ended at `now` met a busy vault or store: the next comes sooner.
    fn met_busy(&mut self, now: Instant) {
        self.regular = self.regular.min(now + self.cadence.busy);
    }
}

/// One pass of the shared files' thread over the writer lineage `paths` belongs to: the
/// publisher, then the materializer, so the snapshot a pass leaves holds what it signed.
/// Each intent this Mac queued becomes one record, or stays queued or held with its reason in
/// the outbox (`aterm file outbox`); with nothing queued the publisher reads three small files
/// and opens no custody. Every materializer outcome is a state word in the file's own
/// `state.json`: nothing to report here, and nothing a later pass does not see for itself.
/// What the publisher signed it notes in `journal` ([`crate::file::publish::Pass::journal`]),
/// this user's spool ([`journal_spool`]) in a running aterm. Returns the materializer's
/// outcomes, one a file.
fn files_pass(
    paths: &Paths,
    config: &std::path::Path,
    publish_by: Instant,
    journal: Option<&Spool>,
) -> Vec<crate::file::materialize::Outcome> {
    let published = crate::file::publish::pass(
        paths,
        config,
        None,
        &crate::file::publish::Pass {
            by: publish_by,
            now_ms: aterm_time::unix_millis().unwrap_or(0),
            journal,
        },
    );
    drop(published);
    crate::file::materialize::pass(paths, config, None, &crate::file::materialize::Pass::now())
}

/// Whether a pass's `outcomes` met a busy vault or store.
fn met_busy(outcomes: &[crate::file::materialize::Outcome]) -> bool {
    outcomes
        .iter()
        .any(|outcome| outcome.why == Some(aterm_file::Why::Busy))
}

/// Whether another process of this Mac owns the Commons runtime, so the materializer rests
/// ([`Materializer`]): the owner's loop sets it at each report, and a flip wakes the
/// materializer's wait ([`crate::file::growth::Growth::ring`]). Its flag is private to this
/// module, so no flip escapes the ring.
#[derive(Debug, Default)]
pub(crate) struct Elsewhere(AtomicBool);

impl Elsewhere {
    /// Another process owns the Commons runtime (`true`), or none does now (`false`).
    pub(crate) fn set(&self, elsewhere: bool) {
        if self.0.swap(elsewhere, Ordering::AcqRel) != elsewhere {
            GROWTH.ring();
        }
    }

    fn is_set(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// What the materializer waits on ([`materialize`]): this process's clock and its stores' growth
/// ([`Live`]), or a test's script of both.
trait Bell {
    fn now(&self) -> Instant;

    /// Wait until the growth count leaves `seen` (`None`: no growth wakes this wait), `woke`
    /// holds, or `until` comes (`None`: no deadline), and return the count read last
    /// ([`crate::file::growth::Growth::wait`]).
    fn wait(&self, until: Option<Instant>, seen: Option<u64>, woke: &dyn Fn() -> bool) -> u64;
}

/// This process's clock and growth ([`GROWTH`]).
struct Live;

impl Bell for Live {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wait(&self, until: Option<Instant>, seen: Option<u64>, woke: &dyn Fn() -> bool) -> u64 {
        GROWTH.wait(until, seen, woke)
    }
}

/// The materializer's thread until `stop` ([`Materializer`]): a pass whenever one is due
/// ([`Files::due`]; `pass` answers whether it met a busy vault or store), and between passes one
/// wait, which wakes for what it waits for and nothing else: the first growth since the last pass
/// started, the pass due next, a flip of `elsewhere` and the stop. Only a wait that growth can wake
/// asks for growth ([`Files::awaits`]): the appends of a burst inside the settle, and any while
/// the thread rests, ring nothing. While `elsewhere` is set it waits with no deadline; once it
/// clears, a pass that came due meanwhile, or the growth it saw, is taken up at once.
fn materialize(
    files: &mut Files,
    bell: &dyn Bell,
    stop: &AtomicBool,
    elsewhere: &Elsewhere,
    pass: &mut dyn FnMut() -> bool,
) {
    loop {
        let resting = elsewhere.is_set();
        let until = (!resting).then(|| files.next_at());
        let seen = files.awaits().filter(|_| !resting);
        let growth = bell.wait(until, seen, &|| {
            stop.load(Ordering::Acquire) || elsewhere.is_set() != resting
        });
        if stop.load(Ordering::Acquire) {
            return;
        }
        if elsewhere.is_set() {
            continue;
        }
        if files.due(bell.now(), growth) && pass() {
            files.met_busy(bell.now());
        }
    }
}

/// The shared files' materializer of one Commons runtime owner
/// (docs/DESIGN-shared-files-2026-11-11.md §4): a thread of its own, `alab-files`, that lives as
/// long as the owner does and passes on its [`Cadence`] whatever the owner is doing. The words a
/// reader of a shared file needs are the ones custody refuses under (`[commons]` off, no
/// membership, a succession that fences this Mac's home or serves a candidate nobody admitted),
/// and those are the states in which the owner is not serving: a pass run only by a serving
/// owner would leave the last `live` standing through every one of them. Between passes the
/// thread sleeps ([`materialize`]).
///
/// It rests while `elsewhere` is set: another process of this Mac owns the Commons runtime
/// then, and its materializer passes.
pub(crate) struct Materializer {
    stop: Arc<AtomicBool>,
    running: Option<JoinHandle<()>>,
}

impl Materializer {
    /// Start the materializer over the writer lineage `paths` belongs to, reading `[commons]`
    /// from `config`, its publisher noting what it signed in `journal` ([`files_pass`]). `None`
    /// where its thread could not start: the files then move only by `aterm file refresh`.
    pub(crate) fn start(
        paths: Paths,
        config: std::path::PathBuf,
        cadence: Cadence,
        elsewhere: Arc<Elsewhere>,
        journal: Option<Spool>,
    ) -> Option<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        // Read here, so a store that grows once this returns is growth the thread wakes on.
        let mut files = Files::new(cadence, Instant::now(), GROWTH.count());
        let running = std::thread::Builder::new()
            .name("alab-files".into())
            .spawn(move || {
                materialize(&mut files, &Live, &stopped, &elsewhere, &mut || {
                    met_busy(&files_pass(
                        &paths,
                        &config,
                        crate::admission::deadline(),
                        journal.as_ref(),
                    ))
                });
            })
            .ok()?;
        Some(Self {
            stop,
            running: Some(running),
        })
    }
}

impl Drop for Materializer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        GROWTH.ring();
        if let Some(running) = self.running.take()
            && running.thread().id() != std::thread::current().id()
        {
            let _ = running.join();
        }
    }
}

/// What this Mac's `sum` row states: its membership label and macOS major version, read in
/// process with no network. The label is the vault's for this Mac's own recipient, never one a
/// host name suggests, since a host name is no identity (the rule `super::register` keeps).
fn presence_live(paths: &Paths) -> Presence {
    Presence {
        label: aterm_secrets::client::resolve_local_swarm_label(paths, &[])
            .ok()
            .flatten()
            .and_then(|label| Word::new(&label.to_ascii_lowercase())),
        os: if cfg!(target_os = "macos") {
            aterm_sysprobe::machine_words().os_major()
        } else {
            None
        },
    }
}

/// The upkeep one serving owner runs: the journal's flusher, or nothing in this crate's tests.
/// The shared files' materializer is not here: it runs for as long as the owner lives, serving
/// or not ([`Materializer`]).
pub(crate) struct Upkeep {
    journal: Option<Journal>,
    secrets: Option<Secrets>,
}

impl Upkeep {
    /// The upkeep of an owner that starts serving at `now`.
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            journal: journal_spool().map(|spool| Journal {
                spool,
                next: now + FIRST,
                running: None,
            }),
            // This crate's tests run their labs' runtimes without touching a vault's store.
            secrets: (!cfg!(test)).then(|| Secrets {
                next: now + FIRST,
                running: None,
            }),
        }
    }

    /// Collect a finished journal pass, then start the next one when it is due; the same for
    /// the registry store's sweep.
    pub(crate) fn tick(&mut self, paths: &Paths, now: Instant) {
        if let Some(journal) = &mut self.journal {
            journal.tick(paths, now);
        }
        if let Some(secrets) = &mut self.secrets {
            secrets.tick(paths, now);
        }
    }
}

/// A sweep of the registry's secret store at most this often ([`Secrets`]).
pub(crate) const SECRETS_EVERY: Duration = Duration::from_secs(60 * 60);

/// The registry store's sweep on its own `alab-secrets` thread
/// ([`super::secret::housekeep`]): what an ended run left owed (a failed removal, a refused
/// write's put, a late roll's dropped entries) is collected once no cell names it, though this
/// Mac writes no secret again. A pass reads this Mac's owed record and, only where it lists an
/// ended run's entry, the registry. Every [`SECRETS_EVERY`], [`RETRY`] after one that failed.
struct Secrets {
    next: Instant,
    running: Option<JoinHandle<bool>>,
}

impl Secrets {
    fn tick(&mut self, paths: &Paths, now: Instant) {
        if self.running.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(handle) = self.running.take()
        {
            let swept = handle.join().unwrap_or(false);
            self.next = now + if swept { SECRETS_EVERY } else { RETRY };
        }
        if self.running.is_some() || now < self.next {
            return;
        }
        let paths = paths.clone();
        self.running = std::thread::Builder::new()
            .name("alab-secrets".into())
            .spawn(move || {
                let handle = crate::att::Handle::at(paths.clone(), &crate::att::REGISTRY);
                let store = super::secret::store(&paths);
                super::secret::housekeep(&handle, &store).is_ok()
            })
            .ok();
        if self.running.is_none() {
            self.next = now + RETRY;
        }
    }
}

#[cfg(test)]
#[path = "upkeep_tests.rs"]
pub(crate) mod tests;
