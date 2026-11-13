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
