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
//! what another process of this Mac appended and for what custody refuses now. It passes
//! whether or not the owner is serving, and each pass writes only under `~/.aterm/files`. The
//! same thread runs the shared files' publisher before each pass (`crate::file::publish`): the
//! writes this Mac queued are signed there, one record each.

use super::journal::{Presence, Spool, Word, flush::Flushed};
use aterm_secrets::client::Paths;
use std::{
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

/// The journal's spool this build flushes: this user's (`Spool::local`), or none in this crate's
/// tests, whose runtime labs must not touch the host's state.
fn journal_spool() -> Option<Spool> {
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
                super::journal::flush::pass_with(
                    &paths,
                    &spool,
                    &|| presence_live(&paths),
                    &super::journal::place::holdings,
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
    /// How often the materializer looks whether a pass is due.
    pub(crate) look: Duration,
}

impl Cadence {
    /// The cadence of a running aterm.
    pub(crate) const LIVE: Self = Self {
        first: Duration::from_secs(5),
        settle: Duration::from_secs(2),
        every: Duration::from_secs(60),
        busy: Duration::from_secs(10),
        look: Duration::from_secs(1),
    };
}

/// Every this many passes projects again though no record arrived, which takes up what changes
/// without one (a membership record that makes an owned row readable).
pub(crate) const FILES_AGAIN: u32 = 5;

/// When the shared files' materializer is due: a pass shortly after a table store of this
/// process grew, and one each [`Cadence::every`] besides.
struct Files {
    cadence: Cadence,
    /// The growth count the last pass started at.
    seen: u64,
    /// When a look first saw the count past `seen`.
    moved_at: Option<Instant>,
    /// When the next pass is due whether or not a store grew.
    regular: Instant,
    passes: u32,
}

impl Files {
    /// The schedule of an owner that starts at `now`: its first pass, which projects whatever
    /// the snapshot holds, is due [`Cadence::first`] later.
    fn new(cadence: Cadence, now: Instant) -> Self {
        Self {
            cadence,
            seen: crate::file::growth(),
            moved_at: None,
            regular: now + cadence.first,
            passes: FILES_AGAIN - 1,
        }
    }

    /// Whether a pass is due at `now`, the growth count reading `growth`: `Some(again)` starts
    /// one, `again` saying it projects though nothing arrived. A pass is due
    /// [`Cadence::settle`] after a store was seen to have grown, and never later than the
    /// regular one.
    fn due(&mut self, now: Instant, growth: u64) -> Option<bool> {
        if growth != self.seen && self.moved_at.is_none() {
            self.moved_at = Some(now);
        }
        let due = self.moved_at.map_or(self.regular, |moved| {
            (moved + self.cadence.settle).min(self.regular)
        });
        if now < due {
            return None;
        }
        let again = self.passes % FILES_AGAIN == FILES_AGAIN - 1;
        self.seen = growth;
        self.moved_at = None;
        self.regular = now + self.cadence.every;
        self.passes = self.passes.wrapping_add(1);
        Some(again)
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
/// Returns whether the pass met a busy vault or store.
fn files_pass(paths: &Paths, config: &std::path::Path, again: bool, publish_by: Instant) -> bool {
    let published = crate::file::publish::pass(
        paths,
        config,
        None,
        &crate::file::publish::Pass {
            by: publish_by,
            now_ms: aterm_time::unix_millis().unwrap_or(0),
        },
    );
    drop(published);
    crate::file::materialize::pass(
        paths,
        config,
        None,
        &crate::file::materialize::Pass::now(again),
    )
    .iter()
    .any(|outcome| outcome.why == Some(aterm_file::Why::Busy))
}

/// The shared files' materializer of one Commons runtime owner
/// (docs/DESIGN-shared-files-2026-11-11.md §4): a thread of its own, `alab-files`, that lives as
/// long as the owner does and passes on its [`Cadence`] whatever the owner is doing. The words a
/// reader of a shared file needs are the ones custody refuses under (`[commons]` off, no
/// membership, a succession that fences this Mac's home or serves a candidate nobody admitted),
/// and those are the states in which the owner is not serving: a pass run only by a serving
/// owner would leave the last `live` standing through every one of them.
///
/// It rests while `elsewhere` is set: another process of this Mac owns the Commons runtime
/// then, and its materializer passes.
pub(crate) struct Materializer {
    stop: std::sync::Arc<aterm_uds::stop::Stop>,
    running: Option<JoinHandle<()>>,
}

impl Materializer {
    /// Start the materializer over the writer lineage `paths` belongs to, reading `[commons]`
    /// from `config`. `None` where its thread could not start: the files then move only by
    /// `aterm file refresh`.
    pub(crate) fn start(
        paths: Paths,
        config: std::path::PathBuf,
        cadence: Cadence,
        elsewhere: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Option<Self> {
        let stop = std::sync::Arc::new(aterm_uds::stop::Stop::new());
        let stopped = stop.clone();
        let running = std::thread::Builder::new()
            .name("alab-files".into())
            .spawn(move || {
                let mut files = Files::new(cadence, Instant::now());
                while !stopped.sleep(cadence.look) {
                    if elsewhere.load(std::sync::atomic::Ordering::Relaxed) {
                        continue;
                    }
                    let Some(again) = files.due(Instant::now(), crate::file::growth()) else {
                        continue;
                    };
                    if files_pass(&paths, &config, again, crate::admission::deadline()) {
                        files.met_busy(Instant::now());
                    }
                }
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
        self.stop.set();
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
        }
    }

    /// Collect a finished journal pass, then start the next one when it is due.
    pub(crate) fn tick(&mut self, paths: &Paths, now: Instant) {
        if let Some(journal) = &mut self.journal {
            journal.tick(paths, now);
        }
    }
}

#[cfg(test)]
#[path = "upkeep_tests.rs"]
mod tests;
